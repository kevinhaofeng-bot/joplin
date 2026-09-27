//! `APP_LITE_SERVER_TOKEN=<secret> app-lite-server --root <dir> --listen <addr>`
//!
//! The token comes from the environment so it never appears in process
//! listings or shell history of the command line.

use std::sync::Arc;

use app_lite_server::{ServerStore, http::HttpServer};

fn main() {
    let mut root = None;
    let mut listen = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--root" => root = args.next(),
            "--listen" => listen = args.next(),
            _ => fail(&format!("unknown argument {arg}")),
        }
    }
    let (Some(root), Some(listen)) = (root, listen) else {
        fail("usage: app-lite-server --root <dir> --listen <addr>");
    };
    let token = std::env::var("APP_LITE_SERVER_TOKEN")
        .unwrap_or_else(|_| fail("APP_LITE_SERVER_TOKEN is not set"));
    let store = ServerStore::open(std::path::Path::new(&root))
        .unwrap_or_else(|error| fail(&format!("cannot open store: {error}")));
    let server = HttpServer::bind(&listen, Arc::new(store), token)
        .unwrap_or_else(|error| fail(&format!("cannot listen: {error}")));
    eprintln!("app-lite-server listening on {}", server.local_addr());
    loop {
        std::thread::park();
    }
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(2);
}
