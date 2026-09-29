//! 背景图 GPU 渲染：解码 + 一次性离屏高斯模糊缓存（spec §6.7 FR-THEME-05）。
//!
//! spec 技术要点：「背景模糊用离屏一次性高斯模糊缓存纹理，不做逐帧后处理」，
//! 「图片解码一次缓存」。本模块：
//! - [`blur_rgba`]：可分离 box-blur 跑 3 趟近似高斯（纯函数，有单测），不新增依赖。
//! - [`decode_and_blur`]：解码 + 可选模糊（后台线程调用），失败返回 `None`。
//! - [`BackgroundImageCache`]：以 `(路径, 模糊半径)` 为键缓存解码后的
//!   [`gpui::RenderImage`]；键变化才需重算，渲染帧零模糊开销（仅取已缓存纹理）。
//!
//! 优雅回退：解码失败/路径无效时缓存为 `None`，渲染层只保留 `palette.background`
//! 纯色（见 `workspace_view` 的背景色填充），不弹窗、不崩溃。
//!
//! 编排（后台线程跑解码、回填缓存、触发重绘）在 `WorkspaceView` 侧完成，保持本模块
//! 的缓存/模糊逻辑可独立单测。

use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use gpui::RenderImage;
use image::{Frame, RgbaImage};

/// 高斯模糊近似：可分离 box-blur，水平 + 垂直各 3 趟。
///
/// `radius <= 0.0` 时直接返回原图（恒等）。box-blur 三趟迭代的数学期望逼近
/// 高斯（中心极限定理），视觉上与高斯模糊等价，但只需 O(n) 整数运算、无 FFT，
/// 适合「一次性离屏」场景。输出尺寸与输入一致，边界按 clamp 处理。
///
/// 这是纯函数、平台无关，便于单测。
pub fn blur_rgba(mut input: RgbaImage, radius: f32) -> RgbaImage {
    if radius <= 0.0 {
        return input;
    }
    let r = radius.round().max(1.0) as usize;
    for _ in 0..3 {
        blur_axis(&mut input, r, true);
        blur_axis(&mut input, r, false);
    }
    input
}

/// 沿单一轴向做一趟 box-blur（半径 r，含两端共 2r+1 个采样）。
/// `horizontal=true` 沿 x，否则沿 y。边界用 clamp（重复边缘像素）。
///
/// 边界窗口比 `2r+1` 小（被 clamp 到 `[0,len)`），故除数取**实际采样数**，
/// 保证纯色图模糊后不变。
fn blur_axis(image: &mut RgbaImage, r: usize, horizontal: bool) {
    let (width, height) = image.dimensions();
    let len = if horizontal { width } else { height } as usize;
    let mut row_buf: Vec<[u32; 4]> = Vec::with_capacity(len);
    row_buf.resize(len, [0; 4]);

    let other_dim = if horizontal { height } else { width };
    for fix in 0..other_dim {
        // 取出该轴一行的像素（u32 累加防溢出：每通道 ≤255，核宽 ≤129，和 ≤ 32895）。
        for (i, slot) in row_buf.iter_mut().enumerate() {
            let px = sample(image, fix, i, horizontal, width, height);
            *slot = [px[0] as u32, px[1] as u32, px[2] as u32, px[3] as u32];
        }
        // 逐像素重算窗口 [i-r, i+r]（clamp 到 [0,len)）的均值并写回。
        // len 一般 ≤ 原图边长，逐像素重算稳健且足够快（一次性离屏，非逐帧）。
        for i in 0..len {
            let lo = i.saturating_sub(r);
            let hi = (i + r + 1).min(len);
            let count = (hi - lo) as u32; // 边界处 < 2r+1，按实际采样数归一化
            let mut sum = [0u32; 4];
            for slot in row_buf.iter().take(hi).skip(lo) {
                for c in 0..4 {
                    sum[c] += slot[c];
                }
            }
            let out = [
                (sum[0] / count) as u8,
                (sum[1] / count) as u8,
                (sum[2] / count) as u8,
                (sum[3] / count) as u8,
            ];
            write_pixel(image, fix, i, horizontal, width, height, out);
        }
    }
}

#[inline]
fn sample(
    image: &RgbaImage,
    fix: u32,
    idx: usize,
    horizontal: bool,
    width: u32,
    height: u32,
) -> [u8; 4] {
    let (x, y) = if horizontal {
        (idx as u32, fix)
    } else {
        (fix, idx as u32)
    };
    image.get_pixel(x.min(width - 1), y.min(height - 1)).0
}

#[inline]
fn write_pixel(
    image: &mut RgbaImage,
    fix: u32,
    idx: usize,
    horizontal: bool,
    width: u32,
    height: u32,
    rgba: [u8; 4],
) {
    let (x, y) = if horizontal {
        (idx as u32, fix)
    } else {
        (fix, idx as u32)
    };
    image.get_pixel_mut(x.min(width - 1), y.min(height - 1)).0 = rgba;
}

/// 把一张 RGBA 图包成 gpui 可绘制的 [`RenderImage`]（单帧）。
///
/// `RenderImage::new` 接收 `impl Into<SmallVec<[Frame; 1]>>`；用 `Vec` 传入，借
/// `From<Vec<T>> for SmallVec` 转换，避免在 termior 直接声明 `smallvec` 依赖。
pub fn render_image_from_rgba(image: RgbaImage) -> Arc<RenderImage> {
    let frame = Frame::new(image);
    Arc::new(RenderImage::new(vec![frame]))
}

/// 解码 + 可选模糊（在后台线程调用）。失败/缺失返回 `None`（优雅回退到纯色）。
pub fn decode_and_blur(path: &Path, blur: f32) -> Option<Arc<RenderImage>> {
    if !path.is_file() {
        log::warn!("background image not found: {}", path.display());
        return None;
    }
    match image::open(path) {
        Ok(image) => {
            let rgba = image.into_rgba8();
            let blurred = blur_rgba(rgba, blur);
            Some(render_image_from_rgba(blurred))
        }
        Err(error) => {
            log::warn!(
                "background image decode failed ({}): {error}",
                path.display()
            );
            None
        }
    }
}

/// 背景图纹理缓存。键为 `(路径, 模糊半径)`；命中直接复用，变化才需重算。
///
/// 不自己 spawn 线程——编排（后台解码 → [`Self::store`] → 触发重绘）在
/// `WorkspaceView` 侧完成，保持本结构无 GPUI 运行时依赖、可单测。
pub struct BackgroundImageCache {
    key: Option<(PathBuf, f32)>,
    image: Option<Arc<RenderImage>>,
}

impl BackgroundImageCache {
    pub fn new() -> Self {
        Self {
            key: None,
            image: None,
        }
    }

    /// 当前已就绪的纹理（若有）。渲染帧同步读，不阻塞。
    pub fn current(&self) -> Option<Arc<RenderImage>> {
        self.image.clone()
    }

    /// 当前缓存的键（路径 + 模糊半径）。
    pub fn current_key(&self) -> Option<(&Path, f32)> {
        self.key.as_ref().map(|(p, b)| (p.as_path(), *b))
    }

    /// 是否需要为 `(path, blur)` 重新解码。键相同（含模糊半径）则返回 `false`。
    /// 调用方据此决定是否启动后台解码任务。
    pub fn needs(&self, path: &Path, blur: f32) -> bool {
        let blur = blur.clamp(0.0, 64.0);
        self.key.as_ref().map(|(p, b)| (p.as_path(), *b)) != Some((path, blur))
    }

    /// 标记开始为 `(path, blur)` 重算：清旧纹理，避免渲染用过期模糊度的图。
    pub fn begin(&mut self, path: PathBuf, blur: f32) {
        let blur = blur.clamp(0.0, 64.0);
        self.key = Some((path, blur));
        self.image = None;
    }

    /// 回填后台解码结果。键仍匹配才写入（期间键又变了则丢弃过期结果），
    /// 返回是否更新成功（调用方据此决定是否 `cx.notify`）。
    pub fn store(&mut self, path: &Path, blur: f32, image: Option<Arc<RenderImage>>) -> bool {
        let blur = blur.clamp(0.0, 64.0);
        if self.key.as_ref().map(|(p, b)| (p.as_path(), *b)) == Some((path, blur)) {
            self.image = image;
            true
        } else {
            false
        }
    }
}

impl Default for BackgroundImageCache {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageBuffer, Rgba};

    fn solid(w: u32, h: u32, c: [u8; 4]) -> RgbaImage {
        ImageBuffer::from_pixel(w, h, Rgba(c))
    }

    #[test]
    fn blur_radius_zero_is_identity() {
        let img = solid(4, 4, [10, 20, 30, 255]);
        let out = blur_rgba(img.clone(), 0.0);
        assert_eq!(out.dimensions(), img.dimensions());
        assert_eq!(out.into_raw(), img.into_raw());
    }

    #[test]
    fn blur_preserves_dimensions() {
        let img = solid(7, 5, [200, 100, 50, 255]);
        let out = blur_rgba(img, 4.0);
        assert_eq!(out.dimensions(), (7, 5));
    }

    #[test]
    fn blur_solid_color_is_unchanged() {
        // 纯色图模糊后仍是同一纯色（每像素都是该色，均值不变）。
        let c = [123, 45, 67, 255];
        let img = solid(8, 8, c);
        let out = blur_rgba(img, 3.0);
        for px in out.pixels() {
            assert_eq!(px.0, c);
        }
    }

    #[test]
    fn blur_does_not_panic_on_single_pixel() {
        let img = solid(1, 1, [9, 9, 9, 255]);
        let out = blur_rgba(img, 5.0);
        assert_eq!(out.dimensions(), (1, 1));
    }

    #[test]
    fn blur_blends_checkerboard_toward_mid() {
        // 2x2 黑白棋盘，模糊后每像素应介于 0..255 之间（混合）。
        let mut img: RgbaImage = ImageBuffer::new(2, 2);
        img.put_pixel(0, 0, Rgba([0, 0, 0, 255]));
        img.put_pixel(1, 0, Rgba([255, 255, 255, 255]));
        img.put_pixel(0, 1, Rgba([255, 255, 255, 255]));
        img.put_pixel(1, 1, Rgba([0, 0, 0, 255]));
        let out = blur_rgba(img, 2.0);
        // 边界 clamp 下，每像素都是黑白均值附近 → 不应仍是纯 0 或纯 255 的极端。
        for px in out.pixels() {
            let v = px.0[0];
            assert!(v > 0 && v < 255, "expected blended value, got {v}");
        }
    }

    #[test]
    fn render_image_from_rgba_keeps_size() {
        let img = solid(3, 2, [1, 2, 3, 4]);
        let ri = render_image_from_rgba(img);
        assert_eq!(ri.frame_count(), 1);
        let size = ri.size(0);
        assert_eq!(size.width.0, 3);
        assert_eq!(size.height.0, 2);
    }

    #[test]
    fn cache_needs_is_false_for_matching_key() {
        let mut cache = BackgroundImageCache::new();
        cache.begin(PathBuf::from("/a.png"), 2.0);
        assert!(!cache.needs(Path::new("/a.png"), 2.0));
        assert!(cache.needs(Path::new("/a.png"), 3.0));
        assert!(cache.needs(Path::new("/b.png"), 2.0));
    }

    #[test]
    fn cache_begin_clears_image() {
        let mut cache = BackgroundImageCache::new();
        cache.image = Some(render_image_from_rgba(solid(1, 1, [0, 0, 0, 255])));
        cache.begin(PathBuf::from("/a.png"), 5.0);
        assert!(cache.current().is_none(), "begin must clear stale texture");
    }

    #[test]
    fn cache_store_rejects_stale_result() {
        let mut cache = BackgroundImageCache::new();
        cache.begin(PathBuf::from("/a.png"), 1.0);
        // 期间键变了 → 旧解码结果应被丢弃。
        cache.begin(PathBuf::from("/a.png"), 9.0);
        let accepted = cache.store(
            Path::new("/a.png"),
            1.0,
            Some(render_image_from_rgba(solid(1, 1, [0, 0, 0, 255]))),
        );
        assert!(!accepted, "stale result must be dropped");
        assert!(cache.current().is_none());
    }

    #[test]
    fn cache_store_accepts_matching_key() {
        let mut cache = BackgroundImageCache::new();
        cache.begin(PathBuf::from("/a.png"), 1.0);
        let tex = render_image_from_rgba(solid(1, 1, [0, 0, 0, 255]));
        let accepted = cache.store(Path::new("/a.png"), 1.0, Some(tex.clone()));
        assert!(accepted);
        assert!(cache.current().is_some());
    }

    #[test]
    fn decode_missing_path_returns_none() {
        assert!(decode_and_blur(Path::new("/definitely/not/here.png"), 0.0).is_none());
    }
}
