//! Linux GPU 探测：在 GPUI/wgpu 启动前避开 Mesa EGL/DRI2 的失败探测。
//!
//! wgpu（`gpui_wgpu`）会同时枚举 Vulkan 和 GLES。没有可访问的 DRM render node
//! 时（QEMU Cirrus/无 3D、未加入 `video`/`render` 组、只有 `card*` 没有
//! `renderD*`），Mesa 的 EGL `eglInitialize` 会打出 `0x3003` /
//! `ZINK: failed to choose pdev` 这类看起来像致命错误的日志，尽管 lavapipe
//! Vulkan 仍能正常出窗。
//!
//! 此时设置 `LIBGL_ALWAYS_SOFTWARE=1`，让 GLES 探测走 llvmpipe，错误日志消失。
//! 已有该环境变量或存在可打开的 `/dev/dri/renderD*` 时不改动。

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

    pub(super) fn prepare(prefer_software: bool) {
        if !should_force_software_gl(
            prefer_software,
            std::env::var_os("LIBGL_ALWAYS_SOFTWARE").is_some(),
            has_accessible_render_node_in(Path::new("/dev/dri")),
        ) {
            return;
        }
        // 进程入口、GPUI 尚未起线程。edition 2021 下 `set_var` 仍是安全函数。
        std::env::set_var("LIBGL_ALWAYS_SOFTWARE", "1");
        log::info!(
            "No accessible DRM render node; using Mesa software GL so wgpu's GLES probe does not fail"
        );
    }
}

// 这两个探测辅助只被上方 cfg(linux|freebsd) 的 `linux::prepare` 调用；其余平台
// 编译期就是死代码，仅由单元测试覆盖，故只在那些平台豁免 dead_code 检查。
#[cfg_attr(not(any(target_os = "linux", target_os = "freebsd")), allow(dead_code))]
pub(crate) fn should_force_software_gl(
    prefer_software: bool,
    libgl_already_set: bool,
    has_accessible_render_node: bool,
) -> bool {
    if libgl_already_set {
        return false;
    }
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
    fn software_gl_is_forced_without_render_node_or_when_preferred() {
        assert!(should_force_software_gl(false, false, false));
        assert!(should_force_software_gl(true, false, true));
        assert!(!should_force_software_gl(false, false, true));
        assert!(!should_force_software_gl(true, true, false));
        assert!(!should_force_software_gl(false, true, false));
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
