//! HTTP Authentication middleware.

use std::marker::PhantomData;
use std::sync::Arc;

use actix_service::{Service, Transform};
use actix_web::dev::{ServiceRequest, ServiceResponse};
use actix_web::Error;
use futures::future::{self, Ready, LocalBoxFuture};
use futures::compat::Future01CompatExt;
use futures::{task::{Context, Poll}, Future, FutureExt, TryFutureExt};
use futures_locks::Mutex;

use crate::extractors::{basic, bearer, AuthExtractor};

/// Middleware for checking HTTP authentication.
///
/// If there is no `Authorization` header in the request,
/// this middleware returns an error immediately,
/// without calling the `F` callback.
///
/// Otherwise, it will pass both the request and
/// the parsed credentials into it.
/// In case of successful validation `F` callback
/// is required to return the `ServiceRequest` back.
#[derive(Debug, Clone)]
pub struct HttpAuthentication<T, F>
where
    T: AuthExtractor,
{
    process_fn: Arc<F>,
    _extractor: PhantomData<T>,
}

impl<T, F, O> HttpAuthentication<T, F>
where
    T: AuthExtractor,
    F: Fn(ServiceRequest, T) -> O,
    O: Future<Output = Result<ServiceRequest, Error>>,
{
    /// Construct `HttpAuthentication` middleware
    /// with the provided auth extractor `T` and
    /// validation callback `F`.
    pub fn with_fn(process_fn: F) -> HttpAuthentication<T, F> {
        HttpAuthentication {
            process_fn: Arc::new(process_fn),
            _extractor: PhantomData,
        }
    }
}

impl<F, O> HttpAuthentication<basic::BasicAuth, F>
where
    F: Fn(ServiceRequest, basic::BasicAuth) -> O,
    O: Future<Output = Result<ServiceRequest, Error>>,
{
    /// Construct `HttpAuthentication` middleware for the HTTP "Basic"
    /// authentication scheme.
    ///
    /// ## Example
    ///
    /// ```rust
    /// # use actix_web::Error;
    /// # use actix_web::dev::ServiceRequest;
    /// # use futures::future;
    /// # use actix_web_httpauth::middleware::HttpAuthentication;
    /// # use actix_web_httpauth::extractors::basic::BasicAuth;
    /// // In this example validator returns immediately,
    /// // but since it is required to return anything
    /// // that implements `IntoFuture` trait,
    /// // it can be extended to query database
    /// // or to do something else in a async manner.
    /// async fn validator(
    ///     req: ServiceRequest,
    ///     credentials: BasicAuth,
    /// ) -> Result<ServiceRequest, Error> {
    ///     // All users are great and more than welcome!
    ///     Ok(req)
    /// }
    ///
    /// let middleware = HttpAuthentication::basic(validator);
    /// ```
    pub fn basic(process_fn: F) -> Self {
        Self::with_fn(process_fn)
    }
}

impl<F, O> HttpAuthentication<bearer::BearerAuth, F>
where
    F: Fn(ServiceRequest, bearer::BearerAuth) -> O,
    O: Future<Output = Result<ServiceRequest, Error>>,
{
    /// Construct `HttpAuthentication` middleware for the HTTP "Bearer"
    /// authentication scheme.
    ///
    /// ## Example
    ///
    /// ```rust
    /// # use actix_web::Error;
    /// # use actix_web::dev::ServiceRequest;
    /// # use futures::future;
    /// # use actix_web_httpauth::middleware::HttpAuthentication;
    /// # use actix_web_httpauth::extractors::bearer::{Config, BearerAuth};
    /// # use actix_web_httpauth::extractors::{AuthenticationError, AuthExtractorConfig};
    /// async fn validator(req: ServiceRequest, credentials: BearerAuth) -> Result<ServiceRequest, Error> {
    ///     if credentials.token() == "mF_9.B5f-4.1JqM" {
    ///         Ok(req)
    ///     } else {
    ///         let config = req.app_data::<Config>()
    ///             .map(|data| data.get_ref().clone())
    ///             .unwrap_or_else(Default::default)
    ///             .scope("urn:example:channel=HBO&urn:example:rating=G,PG-13");
    ///
    ///         Err(AuthenticationError::from(config).into())
    ///     }
    /// }
    ///
    /// let middleware = HttpAuthentication::bearer(validator);
    /// ```
    pub fn bearer(process_fn: F) -> Self {
        Self::with_fn(process_fn)
    }
}

impl<S, B, T, F, O> Transform<S> for HttpAuthentication<T, F>
where
    S: Service<
            Request = ServiceRequest,
            Response = ServiceResponse<B>,
            Error = Error,
        > + 'static,
    S::Future: 'static,
    F: Fn(ServiceRequest, T) -> O + 'static,
    O: Future<Output= Result<ServiceRequest, Error>> + 'static,
    T: AuthExtractor + 'static,
{
    type Request = ServiceRequest;
    type Response = ServiceResponse<B>;
    type Error = Error;
    type Transform = AuthenticationMiddleware<S, F, T>;
    type InitError = ();
    type Future = Ready<Result<Self::Transform, Self::InitError>>;

    fn new_transform(&self, service: S) -> Self::Future {
        future::ok(AuthenticationMiddleware {
            service: Mutex::new(service),
            process_fn: self.process_fn.clone(),
            _extractor: PhantomData,
        })
    }
}

#[doc(hidden)]
pub struct AuthenticationMiddleware<S, F, T>
where
    T: AuthExtractor,
{
    service: Mutex<S>,
    process_fn: Arc<F>,
    _extractor: PhantomData<T>,
}

impl<S, B, F, T, O> Service for AuthenticationMiddleware<S, F, T>
where
    S: Service<
            Request = ServiceRequest,
            Response = ServiceResponse<B>,
            Error = Error,
        > + 'static,
    S::Future: 'static,
    F: Fn(ServiceRequest, T) -> O + 'static,
    O: Future<Output = Result<ServiceRequest, Error>> + 'static,
    T: AuthExtractor + 'static,
{
    type Request = ServiceRequest;
    type Response = ServiceResponse<B>;
    type Error = S::Error;
    type Future = LocalBoxFuture<'static, Result<Self::Response, Self::Error>>;

    fn poll_ready(
        &mut self,
        ctx: &mut Context<'_>,
    ) -> Poll<Result<(), Self::Error>> {
        self.service
            .try_lock()
            .expect("AuthenticationMiddleware was called already")
            .poll_ready(ctx)
    }

    fn call(&mut self, req: Self::Request) -> Self::Future {
        let process_fn = self.process_fn.clone();
        // Note: cloning the mutex, not the service itself
        let inner = self.service.clone();

        extract(req)
            .and_then(move |(req, credentials)| (process_fn)(req, credentials))
            .and_then(move |req| {
                inner
                    .lock()
                    .compat()
                    .map_err(Into::into)
                    .and_then(|mut service| service.call(req))
            })
            .boxed_local()
    }
}

async fn extract<T>(req: ServiceRequest) -> Result<(ServiceRequest, T), Error>
    where
        T: AuthExtractor,
        T::Future: 'static,
        T::Error: 'static,
{
    let credentials = T::from_service_request(&req).await.map_err(Into::into)?;
    Ok((req, credentials))
}
