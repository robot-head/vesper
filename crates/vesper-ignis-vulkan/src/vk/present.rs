//! Presentation to an X11 window through `VK_KHR_xcb_surface` and
//! `VK_KHR_swapchain` (tested under Xvfb with lavapipe).
//!
//! `libxcb` is opened at run time (like the Vulkan loader), so the crate
//! still builds and links without X11 development files; without the
//! library, a display or the surface extensions, the calls return
//! [`VulkanError::Unavailable`]/[`VulkanError::Unsupported`].
//!
//! [`XcbWindow`] and [`Swapchain`] are linear owners like the other GPU
//! objects: released only by their consuming `destroy`, counted by
//! [`crate::leaked_owners`] when dropped.

use super::device::{Device, GpuTexture, Inner, count_leak};
use crate::error::VulkanError;
use crate::golden::Image;
use ash::vk;
use std::ffi::{CString, c_char, c_int, c_uint, c_void};
use std::mem::ManuallyDrop;
use std::sync::Arc;

#[repr(C)]
#[derive(Clone, Copy)]
struct ScreenIterator {
    data: *const Screen,
    rem: c_int,
    index: c_int,
}

#[repr(C)]
struct Screen {
    root: u32,
    default_colormap: u32,
    white_pixel: u32,
    black_pixel: u32,
    current_input_masks: u32,
    width_in_pixels: u16,
    height_in_pixels: u16,
    width_in_mm: u16,
    height_in_mm: u16,
    min_installed_maps: u16,
    max_installed_maps: u16,
    root_visual: u32,
    backing_stores: u8,
    save_unders: u8,
    root_depth: u8,
    allowed_depths_len: u8,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Cookie {
    sequence: c_uint,
}

#[repr(C)]
struct GetImageReply {
    response_type: u8,
    depth: u8,
    sequence: u16,
    length: u32,
    visual: u32,
    pad: [u8; 20],
}

type Conn = c_void;

struct Xcb {
    connect: unsafe extern "C" fn(*const c_char, *mut c_int) -> *mut Conn,
    has_error: unsafe extern "C" fn(*mut Conn) -> c_int,
    get_setup: unsafe extern "C" fn(*mut Conn) -> *const c_void,
    roots: unsafe extern "C" fn(*const c_void) -> ScreenIterator,
    generate_id: unsafe extern "C" fn(*mut Conn) -> u32,
    #[allow(clippy::type_complexity)]
    create_window: unsafe extern "C" fn(
        *mut Conn,
        u8,
        u32,
        u32,
        i16,
        i16,
        u16,
        u16,
        u16,
        u16,
        u32,
        u32,
        *const u32,
    ) -> Cookie,
    request_check: unsafe extern "C" fn(*mut Conn, Cookie) -> *mut c_void,
    map_window: unsafe extern "C" fn(*mut Conn, u32) -> Cookie,
    destroy_window: unsafe extern "C" fn(*mut Conn, u32) -> Cookie,
    flush: unsafe extern "C" fn(*mut Conn) -> c_int,
    get_image: unsafe extern "C" fn(*mut Conn, u8, u32, i16, i16, u16, u16, u32) -> Cookie,
    get_image_reply:
        unsafe extern "C" fn(*mut Conn, Cookie, *mut *mut c_void) -> *mut GetImageReply,
    get_image_data: unsafe extern "C" fn(*const GetImageReply) -> *const u8,
    get_image_data_length: unsafe extern "C" fn(*const GetImageReply) -> c_int,
    disconnect: unsafe extern "C" fn(*mut Conn),
    free: unsafe extern "C" fn(*mut c_void),
    _xcb: libloading::Library,
    _libc: libloading::Library,
}

impl Xcb {
    fn load() -> Result<Self, VulkanError> {
        let unavailable = |e: libloading::Error| VulkanError::Unavailable(format!("libxcb: {e}"));
        // SAFETY: loading the system libxcb and libc runs no unusual
        // initializers; every symbol below is cast to its documented C
        // prototype (xcb.h, xproto.h, stdlib.h).
        unsafe {
            let xcb = libloading::Library::new("libxcb.so.1").map_err(unavailable)?;
            let libc = libloading::Library::new("libc.so.6").map_err(unavailable)?;
            macro_rules! sym {
                ($lib:expr, $name:literal) => {
                    *$lib.get($name).map_err(unavailable)?
                };
            }
            Ok(Xcb {
                connect: sym!(xcb, b"xcb_connect\0"),
                has_error: sym!(xcb, b"xcb_connection_has_error\0"),
                get_setup: sym!(xcb, b"xcb_get_setup\0"),
                roots: sym!(xcb, b"xcb_setup_roots_iterator\0"),
                generate_id: sym!(xcb, b"xcb_generate_id\0"),
                create_window: sym!(xcb, b"xcb_create_window_checked\0"),
                request_check: sym!(xcb, b"xcb_request_check\0"),
                map_window: sym!(xcb, b"xcb_map_window_checked\0"),
                destroy_window: sym!(xcb, b"xcb_destroy_window\0"),
                flush: sym!(xcb, b"xcb_flush\0"),
                get_image: sym!(xcb, b"xcb_get_image\0"),
                get_image_reply: sym!(xcb, b"xcb_get_image_reply\0"),
                get_image_data: sym!(xcb, b"xcb_get_image_data\0"),
                get_image_data_length: sym!(xcb, b"xcb_get_image_data_length\0"),
                disconnect: sym!(xcb, b"xcb_disconnect\0"),
                free: sym!(libc, b"free\0"),
                _xcb: xcb,
                _libc: libc,
            })
        }
    }
}

/// An X11 window on its own XCB connection. See the module docs.
#[must_use = "an XcbWindow must be released with destroy()"]
pub struct XcbWindow {
    xcb: Xcb,
    conn: *mut Conn,
    window: u32,
    width: u32,
    height: u32,
    live: bool,
}

impl std::fmt::Debug for XcbWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("XcbWindow")
            .field("window", &self.window)
            .field("width", &self.width)
            .field("height", &self.height)
            .finish_non_exhaustive()
    }
}

const XCB_WINDOW_CLASS_INPUT_OUTPUT: u16 = 1;
const XCB_CW_BACK_PIXEL: u32 = 2;
const XCB_IMAGE_FORMAT_Z_PIXMAP: u8 = 2;

impl XcbWindow {
    /// Connects to `display` (or `$DISPLAY`) and maps a `width` x `height`
    /// window on the default screen with a black background.
    pub fn open(display: Option<&str>, width: u32, height: u32) -> Result<Self, VulkanError> {
        if width == 0 || height == 0 || width > 8192 || height > 8192 {
            return Err(VulkanError::InvalidDesc("window extent"));
        }
        let xcb = Xcb::load()?;
        let name = display
            .map(|d| CString::new(d).map_err(|_| VulkanError::InvalidDesc("display name")))
            .transpose()?;
        let mut screen_index: c_int = 0;
        // SAFETY: documented XCB calls on the connection we own; pointers
        // returned by XCB are checked before use and freed as documented.
        unsafe {
            let conn = (xcb.connect)(
                name.as_ref().map_or(std::ptr::null(), |n| n.as_ptr()),
                &mut screen_index,
            );
            if conn.is_null() || (xcb.has_error)(conn) != 0 {
                if !conn.is_null() {
                    (xcb.disconnect)(conn);
                }
                return Err(VulkanError::Unavailable(format!(
                    "no X11 display {}",
                    display.map_or("($DISPLAY)".to_owned(), str::to_owned)
                )));
            }
            // Screens are variable-length records; only screen 0 is
            // supported without walking their depth lists.
            if screen_index != 0 {
                (xcb.disconnect)(conn);
                return Err(VulkanError::Unsupported("X11 screens other than 0"));
            }
            let it = (xcb.roots)((xcb.get_setup)(conn));
            if it.data.is_null() || it.rem < 1 {
                (xcb.disconnect)(conn);
                return Err(VulkanError::Window("X11 server has no screen".into()));
            }
            let screen = &*it.data;
            let window = (xcb.generate_id)(conn);
            let values = [screen.black_pixel];
            let cookie = (xcb.create_window)(
                conn,
                0, // XCB_COPY_FROM_PARENT depth
                window,
                screen.root,
                0,
                0,
                width as u16,
                height as u16,
                0,
                XCB_WINDOW_CLASS_INPUT_OUTPUT,
                screen.root_visual,
                XCB_CW_BACK_PIXEL,
                values.as_ptr(),
            );
            let err = (xcb.request_check)(conn, cookie);
            if !err.is_null() {
                (xcb.free)(err);
                (xcb.disconnect)(conn);
                return Err(VulkanError::Window("xcb_create_window failed".into()));
            }
            let err = (xcb.request_check)(conn, (xcb.map_window)(conn, window));
            if !err.is_null() {
                (xcb.free)(err);
                (xcb.destroy_window)(conn, window);
                (xcb.disconnect)(conn);
                return Err(VulkanError::Window("xcb_map_window failed".into()));
            }
            (xcb.flush)(conn);
            Ok(XcbWindow {
                xcb,
                conn,
                window,
                width,
                height,
                live: true,
            })
        }
    }

    pub fn extent(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    /// Reads the window's current contents back from the X server
    /// (`GetImage`, `ZPixmap`), as RGBA with alpha 255. This is what was
    /// presented, as the server composited it.
    pub fn read_pixels(&self) -> Result<Image, VulkanError> {
        // SAFETY: documented XCB calls on our connection and window; the
        // reply is freed with `free` after its data is copied.
        unsafe {
            let cookie = (self.xcb.get_image)(
                self.conn,
                XCB_IMAGE_FORMAT_Z_PIXMAP,
                self.window,
                0,
                0,
                self.width as u16,
                self.height as u16,
                u32::MAX,
            );
            let mut err: *mut c_void = std::ptr::null_mut();
            let reply = (self.xcb.get_image_reply)(self.conn, cookie, &mut err);
            if !err.is_null() {
                (self.xcb.free)(err);
            }
            if reply.is_null() {
                return Err(VulkanError::Window("xcb_get_image failed".into()));
            }
            let depth = (*reply).depth;
            let len = (self.xcb.get_image_data_length)(reply).max(0) as usize;
            let data = (self.xcb.get_image_data)(reply);
            let want = (self.width * self.height * 4) as usize;
            let bytes = if data.is_null() || len < want || depth < 24 {
                None
            } else {
                Some(std::slice::from_raw_parts(data, want).to_vec())
            };
            (self.xcb.free)(reply as *mut c_void);
            let mut bgrx = bytes
                .ok_or_else(|| VulkanError::Window(format!("unexpected image (depth {depth})")))?;
            for px in bgrx.as_chunks_mut::<4>().0 {
                px[3] = 255;
            }
            Image::from_bgra(self.width, self.height, bgrx)
                .map_err(|e| VulkanError::Window(e.to_string()))
        }
    }

    /// Destroys the window and closes the connection. Destroy every
    /// [`Swapchain`] on it first.
    pub fn destroy(mut self) {
        // SAFETY: our window on our connection; nothing uses them afterwards.
        unsafe {
            (self.xcb.destroy_window)(self.conn, self.window);
            (self.xcb.flush)(self.conn);
            (self.xcb.disconnect)(self.conn);
        }
        self.live = false;
    }
}

impl Drop for XcbWindow {
    fn drop(&mut self) {
        if self.live {
            count_leak();
        }
    }
}

/// Parameters of [`Swapchain::new`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SwapchainDesc {
    /// `FIFO` is always available; others are checked.
    pub present_mode: vk::PresentModeKHR,
    /// Images requested beyond the surface minimum.
    pub extra_images: u32,
}

impl Default for SwapchainDesc {
    fn default() -> Self {
        Self {
            present_mode: vk::PresentModeKHR::FIFO,
            extra_images: 1,
        }
    }
}

/// A `VkSwapchainKHR` on an [`XcbWindow`], presenting from the direct
/// queue. See the module docs.
#[must_use = "a Swapchain must be released with destroy()"]
pub struct Swapchain {
    inner: ManuallyDrop<Arc<Inner>>,
    surface: vk::SurfaceKHR,
    swapchain: vk::SwapchainKHR,
    images: Vec<vk::Image>,
    format: vk::Format,
    extent: vk::Extent2D,
    queue: vk::Queue,
    pool: vk::CommandPool,
    commands: Vec<vk::CommandBuffer>,
    /// One acquire semaphore, reusable once the submission waiting on it has
    /// completed (each present waits for its copy's fence).
    acquire: vk::Semaphore,
    /// Per image: signalled by the copy, waited by the present.
    rendered: Vec<vk::Semaphore>,
    fence: vk::Fence,
    /// A copy was submitted with `fence` and has not been seen complete
    /// (its fence wait timed out); the command buffer, acquire semaphore
    /// and source texture are still in use.
    in_flight: bool,
    presents: u64,
    live: bool,
}

impl std::fmt::Debug for Swapchain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Swapchain")
            .field("format", &self.format)
            .field("extent", &self.extent)
            .field("images", &self.images.len())
            .finish_non_exhaustive()
    }
}

impl Swapchain {
    /// Creates a surface for `window` and a swapchain of its size, format
    /// `B8G8R8A8_UNORM` (or the surface's first format), usable as a copy
    /// destination.
    pub fn new(
        device: &Device,
        window: &XcbWindow,
        desc: SwapchainDesc,
    ) -> Result<Self, VulkanError> {
        let inner = device.inner.clone();
        inner.ensure_live("vkCreateXcbSurfaceKHR")?;
        let (Some(xcb), Some(surface_fns), Some(swap_fns)) =
            (&inner.xcb, &inner.surface, &inner.swapchain)
        else {
            return Err(VulkanError::Unsupported(
                "VK_KHR_xcb_surface and VK_KHR_swapchain",
            ));
        };
        let info = vk::XcbSurfaceCreateInfoKHR::default()
            .connection(window.conn)
            .window(window.window);
        // SAFETY: a live connection and window; destroyed on every error
        // path below and in `destroy`.
        let surface = inner.check("vkCreateXcbSurfaceKHR", unsafe {
            xcb.create_xcb_surface(&info, None)
        })?;
        let mut chain = Swapchain {
            inner: ManuallyDrop::new(inner.clone()),
            surface,
            swapchain: vk::SwapchainKHR::null(),
            images: Vec::new(),
            format: vk::Format::UNDEFINED,
            extent: vk::Extent2D::default(),
            queue: inner.queue(inner.direct.index, 0),
            pool: vk::CommandPool::null(),
            commands: Vec::new(),
            acquire: vk::Semaphore::null(),
            rendered: Vec::new(),
            fence: vk::Fence::null(),
            in_flight: false,
            presents: 0,
            live: true,
        };
        // From here on, `chain.release()` cleans up on errors.
        match chain.init(surface_fns, swap_fns, window, desc) {
            Ok(()) => Ok(chain),
            Err(e) => {
                chain.release();
                Err(e)
            }
        }
    }

    fn init(
        &mut self,
        surface_fns: &ash::khr::surface::Instance,
        swap_fns: &ash::khr::swapchain::Device,
        window: &XcbWindow,
        desc: SwapchainDesc,
    ) -> Result<(), VulkanError> {
        let inner = self.inner.clone();
        let d = &inner.device;
        let physical = inner.physical;
        let family = inner.direct.index;
        // SAFETY (block): queries on a live physical device and surface.
        let (supported, caps, formats, modes) = unsafe {
            (
                inner.check(
                    "vkGetPhysicalDeviceSurfaceSupportKHR",
                    surface_fns.get_physical_device_surface_support(physical, family, self.surface),
                )?,
                inner.check(
                    "vkGetPhysicalDeviceSurfaceCapabilitiesKHR",
                    surface_fns.get_physical_device_surface_capabilities(physical, self.surface),
                )?,
                inner.check(
                    "vkGetPhysicalDeviceSurfaceFormatsKHR",
                    surface_fns.get_physical_device_surface_formats(physical, self.surface),
                )?,
                inner.check(
                    "vkGetPhysicalDeviceSurfacePresentModesKHR",
                    surface_fns.get_physical_device_surface_present_modes(physical, self.surface),
                )?,
            )
        };
        if !supported {
            return Err(VulkanError::Unsupported(
                "presentation from the direct queue family",
            ));
        }
        if !modes.contains(&desc.present_mode) {
            return Err(VulkanError::Unsupported("present mode"));
        }
        if !caps
            .supported_usage_flags
            .contains(vk::ImageUsageFlags::TRANSFER_DST)
        {
            return Err(VulkanError::Unsupported(
                "swapchain images as copy destinations",
            ));
        }
        let chosen = formats
            .iter()
            .find(|f| f.format == vk::Format::B8G8R8A8_UNORM)
            .or_else(|| formats.first())
            .copied()
            .ok_or(VulkanError::Unsupported("surface formats"))?;
        let extent = if caps.current_extent.width == u32::MAX {
            vk::Extent2D {
                width: window
                    .width
                    .clamp(caps.min_image_extent.width, caps.max_image_extent.width),
                height: window
                    .height
                    .clamp(caps.min_image_extent.height, caps.max_image_extent.height),
            }
        } else {
            caps.current_extent
        };
        let mut count = caps
            .min_image_count
            .saturating_add(desc.extra_images.min(8));
        if caps.max_image_count != 0 {
            count = count.min(caps.max_image_count);
        }
        let info = vk::SwapchainCreateInfoKHR::default()
            .surface(self.surface)
            .min_image_count(count)
            .image_format(chosen.format)
            .image_color_space(chosen.color_space)
            .image_extent(extent)
            .image_array_layers(1)
            .image_usage(vk::ImageUsageFlags::TRANSFER_DST)
            .image_sharing_mode(vk::SharingMode::EXCLUSIVE)
            .pre_transform(caps.current_transform)
            .composite_alpha(vk::CompositeAlphaFlagsKHR::OPAQUE)
            .present_mode(desc.present_mode)
            .clipped(true);
        // SAFETY (block): valid create infos; every object is released by
        // `release` (null handles are ignored).
        unsafe {
            self.swapchain = inner.check(
                "vkCreateSwapchainKHR",
                swap_fns.create_swapchain(&info, None),
            )?;
            self.images = inner.check(
                "vkGetSwapchainImagesKHR",
                swap_fns.get_swapchain_images(self.swapchain),
            )?;
            self.format = chosen.format;
            self.extent = extent;
            self.pool = inner.check(
                "vkCreateCommandPool",
                d.create_command_pool(
                    &vk::CommandPoolCreateInfo::default()
                        .flags(vk::CommandPoolCreateFlags::RESET_COMMAND_BUFFER)
                        .queue_family_index(family),
                    None,
                ),
            )?;
            self.commands = inner.check(
                "vkAllocateCommandBuffers",
                d.allocate_command_buffers(
                    &vk::CommandBufferAllocateInfo::default()
                        .command_pool(self.pool)
                        .level(vk::CommandBufferLevel::PRIMARY)
                        .command_buffer_count(1),
                ),
            )?;
            self.acquire = inner.check(
                "vkCreateSemaphore",
                d.create_semaphore(&vk::SemaphoreCreateInfo::default(), None),
            )?;
            for _ in 0..self.images.len() {
                let s = inner.check(
                    "vkCreateSemaphore",
                    d.create_semaphore(&vk::SemaphoreCreateInfo::default(), None),
                )?;
                self.rendered.push(s);
            }
            self.fence = inner.check(
                "vkCreateFence",
                d.create_fence(&vk::FenceCreateInfo::default(), None),
            )?;
        }
        Ok(())
    }

    pub fn format(&self) -> vk::Format {
        self.format
    }
    pub fn extent(&self) -> vk::Extent2D {
        self.extent
    }
    pub fn image_count(&self) -> usize {
        self.images.len()
    }
    /// Presents queued so far.
    pub fn presents(&self) -> u64 {
        self.presents
    }

    /// Acquires an image, blits mip 0 / layer 0 of `texture` (scaled to the
    /// swapchain extent, converting the format) into it, and presents it.
    /// Returns the image index after the copy has completed on the GPU (the
    /// presentation itself may still be queued). The texture must be idle
    /// (its owner holds it, so no submitted frame uses it), color, and in a
    /// defined layout; it is left in `TRANSFER_SRC_OPTIMAL`, which its
    /// tracked layout records.
    pub fn present_texture(&mut self, texture: &GpuTexture) -> Result<u32, VulkanError> {
        let inner = self.inner.clone();
        inner.ensure_live("vkAcquireNextImageKHR")?;
        if !Arc::ptr_eq(&texture.inner, &inner) {
            return Err(VulkanError::ForeignDevice);
        }
        if !texture.format().is_color()
            || !texture.usage().contains(vk::ImageUsageFlags::TRANSFER_SRC)
        {
            return Err(VulkanError::InvalidDesc(
                "present source must be a color copy source",
            ));
        }
        // The blit converts into the (unorm/sRGB) swapchain format, which
        // Vulkan allows only from float/unorm/sRGB sources with BLIT_SRC.
        if texture.format() == crate::Format::R32Uint {
            return Err(VulkanError::Unsupported(
                "presenting an integer format (blit to a normalized swapchain)",
            ));
        }
        // SAFETY: valid physical device of this instance.
        let features = unsafe {
            inner
                .instance
                .get_physical_device_format_properties(inner.physical, texture.format().vk())
        }
        .optimal_tiling_features;
        if !features.contains(vk::FormatFeatureFlags::BLIT_SRC) {
            return Err(VulkanError::Unsupported(
                "BLIT_SRC on the present source format",
            ));
        }
        self.wait_previous(&inner)?;
        let old_layout = texture.layout(0, 0).unwrap_or(vk::ImageLayout::UNDEFINED);
        if old_layout == vk::ImageLayout::UNDEFINED {
            return Err(VulkanError::InvalidDesc(
                "present source has undefined contents",
            ));
        }
        let swap = inner
            .swapchain
            .as_ref()
            .ok_or(VulkanError::Unsupported("VK_KHR_swapchain"))?;
        let d = &inner.device;
        // SAFETY (block): every handle is owned by this swapchain or by the
        // caller's idle texture; the single command buffer and acquire
        // semaphore are reused only after `wait_previous` has seen the
        // previous copy's fence; the queue is externally synchronized by
        // `submit_lock`.
        let index = unsafe {
            let index = match swap.acquire_next_image(
                self.swapchain,
                5_000_000_000,
                self.acquire,
                vk::Fence::null(),
            ) {
                Ok((i, _suboptimal)) => i,
                Err(vk::Result::ERROR_OUT_OF_DATE_KHR) => return Err(VulkanError::OutOfDate),
                Err(e) => {
                    return Err(inner
                        .check::<()>("vkAcquireNextImageKHR", Err(e))
                        .unwrap_err());
                }
            };
            let image = self.images[index as usize];
            let cb = self.commands[0];
            inner.check(
                "vkResetCommandBuffer",
                d.reset_command_buffer(cb, vk::CommandBufferResetFlags::empty()),
            )?;
            inner.check(
                "vkBeginCommandBuffer",
                d.begin_command_buffer(
                    cb,
                    &vk::CommandBufferBeginInfo::default()
                        .flags(vk::CommandBufferUsageFlags::ONE_TIME_SUBMIT),
                ),
            )?;
            let color = vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            };
            let to_copy = [
                vk::ImageMemoryBarrier2::default()
                    .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                    .src_access_mask(vk::AccessFlags2::MEMORY_WRITE)
                    .dst_stage_mask(vk::PipelineStageFlags2::BLIT)
                    .dst_access_mask(vk::AccessFlags2::TRANSFER_READ)
                    .old_layout(old_layout)
                    .new_layout(vk::ImageLayout::TRANSFER_SRC_OPTIMAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(texture.image)
                    .subresource_range(color),
                // The acquire semaphore wait (at ALL_COMMANDS) is the first
                // scope; the transition discards the previous contents.
                vk::ImageMemoryBarrier2::default()
                    .src_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                    .src_access_mask(vk::AccessFlags2::NONE)
                    .dst_stage_mask(vk::PipelineStageFlags2::BLIT)
                    .dst_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                    .old_layout(vk::ImageLayout::UNDEFINED)
                    .new_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                    .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                    .image(image)
                    .subresource_range(color),
            ];
            d.cmd_pipeline_barrier2(
                cb,
                &vk::DependencyInfo::default().image_memory_barriers(&to_copy),
            );
            let layers = vk::ImageSubresourceLayers {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                mip_level: 0,
                base_array_layer: 0,
                layer_count: 1,
            };
            let info = texture.info();
            let region = vk::ImageBlit {
                src_subresource: layers,
                src_offsets: [
                    vk::Offset3D::default(),
                    vk::Offset3D {
                        x: info.width as i32,
                        y: info.height as i32,
                        z: 1,
                    },
                ],
                dst_subresource: layers,
                dst_offsets: [
                    vk::Offset3D::default(),
                    vk::Offset3D {
                        x: self.extent.width as i32,
                        y: self.extent.height as i32,
                        z: 1,
                    },
                ],
            };
            d.cmd_blit_image(
                cb,
                texture.image,
                vk::ImageLayout::TRANSFER_SRC_OPTIMAL,
                image,
                vk::ImageLayout::TRANSFER_DST_OPTIMAL,
                &[region],
                vk::Filter::NEAREST,
            );
            let to_present = [vk::ImageMemoryBarrier2::default()
                .src_stage_mask(vk::PipelineStageFlags2::BLIT)
                .src_access_mask(vk::AccessFlags2::TRANSFER_WRITE)
                .dst_stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)
                .dst_access_mask(vk::AccessFlags2::NONE)
                .old_layout(vk::ImageLayout::TRANSFER_DST_OPTIMAL)
                .new_layout(vk::ImageLayout::PRESENT_SRC_KHR)
                .src_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .dst_queue_family_index(vk::QUEUE_FAMILY_IGNORED)
                .image(image)
                .subresource_range(color)];
            d.cmd_pipeline_barrier2(
                cb,
                &vk::DependencyInfo::default().image_memory_barriers(&to_present),
            );
            inner.check("vkEndCommandBuffer", d.end_command_buffer(cb))?;
            let waits = [vk::SemaphoreSubmitInfo::default()
                .semaphore(self.acquire)
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
            let signals = [vk::SemaphoreSubmitInfo::default()
                .semaphore(self.rendered[index as usize])
                .stage_mask(vk::PipelineStageFlags2::ALL_COMMANDS)];
            let cbs = [vk::CommandBufferSubmitInfo::default().command_buffer(cb)];
            let submit = vk::SubmitInfo2::default()
                .wait_semaphore_infos(&waits)
                .command_buffer_infos(&cbs)
                .signal_semaphore_infos(&signals);
            let wait_sems = [self.rendered[index as usize]];
            let chains = [self.swapchain];
            let indices = [index];
            let present = vk::PresentInfoKHR::default()
                .wait_semaphores(&wait_sems)
                .swapchains(&chains)
                .image_indices(&indices);
            let (submitted, presented) = {
                let _guard = inner.submit_lock.lock().unwrap_or_else(|p| p.into_inner());
                let s = d.queue_submit2(self.queue, &[submit], self.fence);
                let p = if s.is_ok() {
                    Some(swap.queue_present(self.queue, &present))
                } else {
                    None
                };
                (s, p)
            };
            inner.check("vkQueueSubmit2", submitted)?;
            // The texture is in TRANSFER_SRC_OPTIMAL once the copy is
            // recorded and submitted.
            if let Some(slot) = texture.slot(0, 0, 0) {
                texture.layouts.borrow_mut()[slot] = vk::ImageLayout::TRANSFER_SRC_OPTIMAL;
            }
            self.in_flight = true;
            self.presents += 1;
            match presented {
                Some(Ok(_suboptimal)) => {}
                Some(Err(vk::Result::ERROR_OUT_OF_DATE_KHR)) => return Err(VulkanError::OutOfDate),
                Some(Err(e)) => inner.check::<()>("vkQueuePresentKHR", Err(e))?,
                None => {}
            }
            index
        };
        self.wait_previous(&inner)?;
        Ok(index)
    }

    /// Waits up to ten seconds for the last copy's fence and resets it. On
    /// timeout the copy stays in flight (nothing is reset or reused) and
    /// [`VulkanError::Pending`] is returned: the source texture may still be
    /// read, so the caller must call [`present_texture`](Self::present_texture)
    /// again or [`destroy`](Self::destroy) (which waits for the queue) before
    /// reusing or destroying it.
    fn wait_previous(&mut self, inner: &Inner) -> Result<(), VulkanError> {
        if !self.in_flight {
            return Ok(());
        }
        let d = &inner.device;
        // SAFETY: our fence; it is reset only after the wait proves the
        // submission that signals it has completed.
        unsafe {
            match d.wait_for_fences(&[self.fence], true, 10_000_000_000) {
                Err(vk::Result::TIMEOUT) => return Err(VulkanError::Pending),
                r => inner.check("vkWaitForFences", r)?,
            }
            inner.check("vkResetFences", d.reset_fences(&[self.fence]))?;
        }
        self.in_flight = false;
        Ok(())
    }

    fn release(&mut self) {
        let inner = self.inner.clone();
        let d = &inner.device;
        // SAFETY: waits for the queue (under the submit lock) so no
        // submitted copy or present uses these objects, then destroys them;
        // null handles are ignored.
        unsafe {
            {
                let _guard = inner.submit_lock.lock().unwrap_or_else(|p| p.into_inner());
                let _ = d.queue_wait_idle(self.queue);
            }
            d.destroy_fence(self.fence, None);
            for s in self.rendered.drain(..) {
                d.destroy_semaphore(s, None);
            }
            d.destroy_semaphore(self.acquire, None);
            d.destroy_command_pool(self.pool, None);
            if let Some(swap) = &inner.swapchain {
                swap.destroy_swapchain(self.swapchain, None);
            }
            if let Some(surface) = &inner.surface {
                surface.destroy_surface(self.surface, None);
            }
            ManuallyDrop::drop(&mut self.inner);
        }
        self.live = false;
    }

    /// Waits for the presentation queue, then destroys the swapchain, its
    /// surface and synchronization objects. The window outlives it.
    pub fn destroy(mut self) {
        self.release();
    }
}

impl Drop for Swapchain {
    fn drop(&mut self) {
        if self.live {
            count_leak();
        }
    }
}
