//! Linux GPU 探测：在 GPUI/wgpu 启动前选择可用的软件 OpenGL 实现。
//!
//! 没有可访问的 DRM render node 时（QEMU Cirrus/无 3D、未加入
//! `video`/`render` 组、只有 `card*` 没有 `renderD*`），使用 Mesa
//! llvmpipe，并避开可能已失效的厂商 EGL 实现。用户显式设置的环境变量优先。

use std::path::Path;

/// 在创建 GPUI/wgpu 实例之前调用。
///
/// `prefer_software` 对应设置里的 [`termior_store::Settings::prefer_software_rendering`]。
pub fn prepare(prefer_software: bool) {
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    linux::prepare(prefer_software);

    #[cfg(not(any(target_os = "linux", target_os = "freebsd")))]
    let _ = prefer_software;
}

#[cfg(any(target_os = "linux", target_os = "freebsd"))]
mod linux {
    use super::*;

    const MESA_EGL_VENDOR: &str = "/usr/share/glvnd/egl_vendor.d/50_mesa.json";

    pub(super) fn prepare(prefer_software: bool) {
        if !should_use_software_rendering(
            prefer_software,
            has_accessible_render_node_in(Path::new("/dev/dri")),
        ) {
            return;
        }
        // 进程入口、GPUI 尚未起线程。edition 2021 下 `set_var` 仍是安全函数。
        if std::env::var_os("LIBGL_ALWAYS_SOFTWARE").is_none() {
            std::env::set_var("LIBGL_ALWAYS_SOFTWARE", "1");
        }
        if std::env::var_os("WGPU_BACKEND").is_none() {
            std::env::set_var("WGPU_BACKEND", "gl");
        }
        if std::env::var_os("MESA_GL_VERSION_OVERRIDE").is_none() {
            std::env::set_var("MESA_GL_VERSION_OVERRIDE", "4.5");
        }
        if std::env::var_os("MESA_GLSL_VERSION_OVERRIDE").is_none() {
            std::env::set_var("MESA_GLSL_VERSION_OVERRIDE", "450");
        }
        if std::env::var_os("__EGL_VENDOR_LIBRARY_FILENAMES").is_none()
            && Path::new(MESA_EGL_VENDOR).is_file()
        {
            std::env::set_var("__EGL_VENDOR_LIBRARY_FILENAMES", MESA_EGL_VENDOR);
        }
        log::info!("Software rendering fallback enabled before GPUI initialization");
    }
}

// 这两个探测辅助只被上方 cfg(linux|freebsd) 的 `linux::prepare` 调用；其余平台
// 编译期就是死代码，仅由单元测试覆盖，故只在那些平台豁免 dead_code 检查。
#[cfg_attr(not(any(target_os = "linux", target_os = "freebsd")), allow(dead_code))]
pub(crate) fn should_use_software_rendering(
    prefer_software: bool,
    has_accessible_render_node: bool,
) -> bool {
    prefer_software || !has_accessible_render_node
}

#[cfg_attr(not(any(target_os = "linux", target_os = "freebsd")), allow(dead_code))]
pub(crate) fn has_accessible_render_node_in(dri: &Path) -> bool {
    let Ok(entries) = std::fs::read_dir(dri) else {
        return false;
    };
    entries.flatten().any(|entry| {
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return false;
        };
        name.starts_with("renderD") && std::fs::File::open(entry.path()).is_ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn software_rendering_is_used_without_render_node_or_when_preferred() {
        assert!(should_use_software_rendering(false, false));
        assert!(should_use_software_rendering(true, true));
        assert!(!should_use_software_rendering(false, true));
    }

    #[test]
    fn only_openable_render_nodes_count() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!has_accessible_render_node_in(dir.path()));

        fs::write(dir.path().join("card0"), []).unwrap();
        assert!(!has_accessible_render_node_in(dir.path()));

        fs::write(dir.path().join("renderD128"), []).unwrap();
        assert!(has_accessible_render_node_in(dir.path()));

        assert!(!has_accessible_render_node_in(Path::new("/no/such/dri")));
    }
}
