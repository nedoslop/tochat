fn main() {
    let mut windows_attrs = tauri_build::WindowsAttributes::new();

    if !cfg!(debug_assertions) {
        windows_attrs = windows_attrs.static_vc_runtime(true);
    }

    let attrs = tauri_build::Attributes::new().windows_attributes(windows_attrs);

    tauri_build::try_build(attrs)
        .expect("failed to run tauri-build");
}
