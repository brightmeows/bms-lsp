//! BMS language server 入口。

mod index;
mod server;

fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    server::run()
}
