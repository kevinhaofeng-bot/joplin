//! `APP_LITE_SERVER_TOKEN=<secret> app-lite-server --root <dir> --listen <addr>`
//!
//! The token comes from the environment so it never appears in process
//! listings or shell history of the command line. With
//! `APP_LITE_TLS_CERT` and `APP_LITE_TLS_KEY` (PEM file paths) the server
//! speaks HTTPS itself; setting only one of them is refused.

use std::sync::Arc;

use app_lite_server::{
    ServerStore,
    http::{HttpOptions, HttpServer, tls_config_from_pem},
};

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
    let tls = match (
        std::env::var_os("APP_LITE_TLS_CERT"),
        std::env::var_os("APP_LITE_TLS_KEY"),
    ) {
        (None, None) => None,
        (Some(cert), Some(key)) => {
            let read = |path: &std::ffi::OsStr| {
                std::fs::read(path).unwrap_or_else(|error| {
                    fail(&format!("cannot read {}: {error}", path.to_string_lossy()))
                })
            };
            Some(
                tls_config_from_pem(&read(&cert), &read(&key)).unwrap_or_else(|error| {
                    fail(&format!("invalid TLS certificate or key: {error}"))
                }),
            )
        }
        _ => fail("set both APP_LITE_TLS_CERT and APP_LITE_TLS_KEY, or neither"),
    };
    let scheme = if tls.is_some() { "https" } else { "http" };
    let options = HttpOptions {
        tls,
        ..Default::default()
    };
    let server = HttpServer::bind_with(&listen, Arc::new(store), token, options)
        .unwrap_or_else(|error| fail(&format!("cannot listen: {error}")));
    eprintln!(
        "app-lite-server listening on {} ({scheme})",
        server.local_addr()
    );
    loop {
        std::thread::park();
    }
}

fn fail(message: &str) -> ! {
    eprintln!("{message}");
    std::process::exit(2);
}
