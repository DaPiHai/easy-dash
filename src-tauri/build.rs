fn main() {
    // 设置 EASY_DASH_NO_ADMIN=1 可构建无需管理员权限的版本（调试/自动化测试用）
    println!("cargo:rerun-if-env-changed=EASY_DASH_NO_ADMIN");
    // app.manifest 通过 include_str! 参与构建脚本编译，Cargo 不会自动追踪它；
    // 显式声明依赖，避免切换 asInvoker/requireAdministrator 后沿用旧的构建脚本输出。
    println!("cargo:rerun-if-changed=app.manifest");
    let mut attrs = tauri_build::Attributes::new();
    if std::env::var("EASY_DASH_NO_ADMIN").is_err() {
        attrs = attrs.windows_attributes(
            tauri_build::WindowsAttributes::new()
                .app_manifest(include_str!("app.manifest")),
        );
    }
    tauri_build::try_build(attrs).expect("failed to run tauri-build");
}
