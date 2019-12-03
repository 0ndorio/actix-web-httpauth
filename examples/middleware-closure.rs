use actix_web::{middleware, web, App, HttpServer};

use futures::future;

use actix_web_httpauth::middleware::HttpAuthentication;

async fn test() -> &'static str {
    "Test\r\n"
}

fn main() -> std::io::Result<()> {
    HttpServer::new(|| {
        let auth = HttpAuthentication::basic(|req, _credentials| future::ok(req));
        App::new()
            .wrap(middleware::Logger::default())
            .wrap(auth)
            .service(web::resource("/").to(test))
    })
    .bind("127.0.0.1:8080")?
    .workers(1)
    .run()
}
