use test_server::TestServer;

#[tokio::main]
async fn main() -> std::io::Result<()> {
    let port = match std::env::args().nth(1) {
        Some(arg) => arg
            .parse()
            .map_err(|_| std::io::Error::other(format!("无效的端口：{arg}")))?,
        None => 9991,
    };
    let server = TestServer::start(port).await?;
    println!("{}", server.url());
    println!("登录：{} / 任意密码", test_server::ME_EMAIL);
    std::future::pending::<()>().await;
    Ok(())
}
