fn main() {
    // 设置 EASY_DASH_NO_ADMIN=1 可构建无需管理员权限的版本（调试/自动化测试用）
    println!("cargo:rerun-if-env-changed=EASY_DASH_NO_ADMIN");
    let mut attrs = tauri_build::Attributes::new();
    if std::env::var("EASY_DASH_NO_ADMIN").is_err() {
        attrs = attrs.windows_attributes(
            tauri_build::WindowsAttributes::new()
                .app_manifest(include_str!("app.manifest")),
        );
    }
    tauri_build::try_build(attrs).expect("failed to run tauri-build");
}
