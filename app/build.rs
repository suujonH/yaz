fn main() {
    println!("cargo:rerun-if-changed=assets/yaz.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        let mut res = winresource::WindowsResource::new();
        res.set_icon("assets/yaz.ico");
        res.set("ProductName", "YaZ");
        res.set("FileDescription", "YaZ");
        if let Err(e) = res.compile() {
            panic!("嵌入 exe 图标失败：{e}");
        }
    }
}
