mod epoll;
mod http_req;
mod http_res;
mod server;

fn main() -> std::io::Result<()> {
    server::run()
}
