//! Instance, device and owned GPU objects.
//!
//! Every owned object (`GpuBuffer`, `GpuTexture`, `Pipeline`,
//! `CommandAllocator`) is a linear host owner: not `Clone`, `#[must_use]`, and
//! released only by its consuming `destroy`. Dropping one instead leaks the
//! Vulkan object (and keeps the device alive) and increments
//! [`leaked_owners`]; it is never silently destroyed, because the GPU might
//! still use it.

use crate::error::VulkanError;
use crate::shader::{BindingKind, CompiledShader, Stage, check_layout};
use ash::{ext, khr, vk};
use std::cell::RefCell;
use std::ffi::{CStr, CString, c_void};
use std::mem::ManuallyDrop;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use vesper_ignis::QueueKind;

static LEAKED: AtomicU64 = AtomicU64::new(0);

/// Number of owners (buffers, textures, pipelines, allocators, backends)
/// dropped without their consuming `destroy` call in this process.
pub fn leaked_owners() -> u64 {
    LEAKED.load(Ordering::Relaxed)
}
pub(crate) fn count_leak() {
    LEAKED.fetch_add(1, Ordering::Relaxed);
}

const VALIDATION_LAYER: &CStr = c"VK_LAYER_KHRONOS_validation";

/// Whether to load `VK_LAYER_KHRONOS_validation` (with synchronization
/// validation enabled).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Validation {
    Off,
    IfAvailable,
    Required,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceConfig {
    pub validation: Validation,
    /// Case-insensitive substring of the device name to select.
    pub device_name: Option<String>,
}

impl Default for DeviceConfig {
    fn default() -> Self {
        Self {
            validation: Validation::IfAvailable,
            device_name: None,
        }
    }
}

impl DeviceConfig {
    /// `VESPER_IGNIS_VULKAN_VALIDATION` = `off` | `required` (default: if
    /// available); `VESPER_IGNIS_VULKAN_DEVICE` = device-name substring.
    pub fn from_env() -> Self {
        let validation = match std::env::var("VESPER_IGNIS_VULKAN_VALIDATION").as_deref() {
            Ok("off") | Ok("0") => Validation::Off,
            Ok("required") | Ok("1") => Validation::Required,
            _ => Validation::IfAvailable,
        };
        Self {
            validation,
            device_name: std::env::var("VESPER_IGNIS_VULKAN_DEVICE").ok(),
        }
    }
}

/// What was selected and enabled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceInfo {
    pub name: String,
    pub driver_name: String,
    pub driver_info: String,
    /// Device API version (major, minor, patch).
    pub api_version: (u32, u32, u32),
    pub device_type: String,
    pub validation: bool,
    pub sync_validation: bool,
    /// (family index, flags, queue count) of the families in use.
    pub families: Vec<(u32, String, u32)>,
}

/// One validation-layer message of warning or error severity.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ValidationMessage {
    pub error: bool,
    pub id: String,
    pub text: String,
}

#[derive(Default)]
struct Messages {
    list: Mutex<Vec<ValidationMessage>>,
}

const MAX_MESSAGES: usize = 4096;

unsafe extern "system" fn debug_callback(
    severity: vk::DebugUtilsMessageSeverityFlagsEXT,
    _types: vk::DebugUtilsMessageTypeFlagsEXT,
    data: *const vk::DebugUtilsMessengerCallbackDataEXT<'_>,
    user: *mut c_void,
) -> vk::Bool32 {
    if data.is_null() || user.is_null() {
        return vk::FALSE;
    }
    // SAFETY: `user` is the `Messages` boxed in `Inner`, which outlives the
    // messenger; `data` is valid for the duration of the callback.
    let (messages, data) = unsafe { (&*(user as *const Messages), &*data) };
    let text = |p: *const std::ffi::c_char| {
        if p.is_null() {
            String::new()
        } else {
            // SAFETY: the layer passes NUL-terminated strings.
            unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
        }
    };
    let message = ValidationMessage {
        error: severity.contains(vk::DebugUtilsMessageSeverityFlagsEXT::ERROR),
        id: text(data.p_message_id_name),
        text: text(data.p_message),
    };
    if std::env::var_os("VESPER_IGNIS_VULKAN_PRINT").is_some() {
        eprintln!("[vulkan] {}: {}", message.id, message.text);
    }
    if let Ok(mut list) = messages.list.lock()
        && list.len() < MAX_MESSAGES
    {
        list.push(message);
    }
    vk::FALSE
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Family {
    pub index: u32,
    pub count: u32,
}

pub(crate) struct Inner {
    _entry: ash::Entry,
    instance: ash::Instance,
    debug: Option<(ext::debug_utils::Instance, vk::DebugUtilsMessengerEXT)>,
    messages: Box<Messages>,
    pub device: ash::Device,
    pub push: khr::push_descriptor::Device,
    pub direct: Family,
    pub compute: Family,
    pub copy: Family,
    /// Distinct family indices in use (resources are CONCURRENT across them).
    pub sharing: Vec<u32>,
    pub queues: Vec<(u32, Vec<vk::Queue>)>,
    memory: vk::PhysicalDeviceMemoryProperties,
    pub limits: vk::PhysicalDeviceLimits,
    info: DeviceInfo,
    lost: AtomicBool,
    /// Serializes host access to the shared `VkQueue`s (external
    /// synchronization), across every backend built on this device.
    pub submit_lock: Mutex<()>,
}

impl Drop for Inner {
    fn drop(&mut self) {
        // SAFETY: every child object holds an `Arc<Inner>`, so none is alive
        // (leaked owners keep the Arc forever and prevent this drop).
        unsafe {
            let _ = self.device.device_wait_idle();
            self.device.destroy_device(None);
            if let Some((utils, messenger)) = self.debug.take() {
                utils.destroy_debug_utils_messenger(messenger, None);
            }
            self.instance.destroy_instance(None);
        }
    }
}

impl Inner {
    pub fn check<T>(&self, call: &'static str, r: Result<T, vk::Result>) -> Result<T, VulkanError> {
        if self.lost.load(Ordering::Acquire) {
            return Err(VulkanError::DeviceLost { call });
        }
        r.map_err(|e| {
            if e == vk::Result::ERROR_DEVICE_LOST {
                self.lost.store(true, Ordering::Release);
            }
            VulkanError::vk(call, e)
        })
    }
    pub fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Acquire)
    }
    /// Fails before a Vulkan call is made once the device is lost, so that
    /// no object is created and then discarded.
    pub fn ensure_live(&self, call: &'static str) -> Result<(), VulkanError> {
        if self.is_lost() {
            Err(VulkanError::DeviceLost { call })
        } else {
            Ok(())
        }
    }
    pub fn family(&self, kind: QueueKind) -> Family {
        match kind {
            QueueKind::Direct => self.direct,
            QueueKind::Compute => self.compute,
            QueueKind::Copy => self.copy,
        }
    }
    pub fn queue(&self, family: u32, index: u32) -> vk::Queue {
        let (_, list) = self
            .queues
            .iter()
            .find(|(f, _)| *f == family)
            .expect("family created");
        list[(index as usize) % list.len()]
    }
    fn memory_type(&self, bits: u32, want: vk::MemoryPropertyFlags) -> Option<u32> {
        (0..self.memory.memory_type_count).find(|i| {
            bits & (1 << i) != 0
                && self.memory.memory_types[*i as usize]
                    .property_flags
                    .contains(want)
        })
    }
    fn sharing<'a, T: SharingInfo<'a>>(&'a self, info: T) -> T {
        if self.sharing.len() > 1 {
            info.concurrent(&self.sharing)
        } else {
            info
        }
    }
}

trait SharingInfo<'a> {
    fn concurrent(self, families: &'a [u32]) -> Self;
}
impl<'a> SharingInfo<'a> for vk::BufferCreateInfo<'a> {
    fn concurrent(self, families: &'a [u32]) -> Self {
        self.sharing_mode(vk::SharingMode::CONCURRENT)
            .queue_family_indices(families)
    }
}
impl<'a> SharingInfo<'a> for vk::ImageCreateInfo<'a> {
    fn concurrent(self, families: &'a [u32]) -> Self {
        self.sharing_mode(vk::SharingMode::CONCURRENT)
            .queue_family_indices(families)
    }
}

/// A Vulkan 1.3 device with synchronization2, timeline semaphores, dynamic
/// rendering and push descriptors. Cheap to clone; destroyed with the last
/// clone after every owned object is destroyed.
#[derive(Clone)]
pub struct Device {
    pub(crate) inner: Arc<Inner>,
}

impl std::fmt::Debug for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Device")
            .field("name", &self.inner.info.name)
            .finish_non_exhaustive()
    }
}

fn unavailable(what: impl std::fmt::Display) -> VulkanError {
    VulkanError::Unavailable(what.to_string())
}

fn family_flags(flags: vk::QueueFlags) -> String {
    format!("{flags:?}")
}

impl Device {
    pub fn new(config: &DeviceConfig) -> Result<Self, VulkanError> {
        // SAFETY: loading the system Vulkan loader runs its initializers; this
        // is the documented use of `Entry::load`.
        let entry =
            unsafe { ash::Entry::load() }.map_err(|e| unavailable(format!("loader: {e}")))?;
        // SAFETY: plain enumeration calls on a loaded entry.
        let layers = unsafe { entry.enumerate_instance_layer_properties() }
            .map_err(|e| unavailable(format!("layers: {e}")))?;
        let has_layer = layers
            .iter()
            .any(|l| l.layer_name_as_c_str().ok() == Some(VALIDATION_LAYER));
        let validation = match config.validation {
            Validation::Off => false,
            Validation::IfAvailable => has_layer,
            Validation::Required if has_layer => true,
            Validation::Required => return Err(VulkanError::ValidationLayerMissing),
        };
        // SAFETY: as above.
        let mut extensions = unsafe { entry.enumerate_instance_extension_properties(None) }
            .map_err(|e| unavailable(format!("instance extensions: {e}")))?;
        if validation {
            // SAFETY: as above; the layer name is NUL-terminated.
            if let Ok(more) =
                unsafe { entry.enumerate_instance_extension_properties(Some(VALIDATION_LAYER)) }
            {
                extensions.extend(more);
            }
        }
        let has_ext = |name: &CStr| {
            extensions
                .iter()
                .any(|e| e.extension_name_as_c_str().ok() == Some(name))
        };
        let debug_utils = validation && has_ext(ext::debug_utils::NAME);
        let sync_validation = validation && has_ext(ext::validation_features::NAME);
        let mut ext_names = Vec::new();
        if debug_utils {
            ext_names.push(ext::debug_utils::NAME.as_ptr());
        }
        if sync_validation {
            ext_names.push(ext::validation_features::NAME.as_ptr());
        }
        let layer_names = if validation {
            vec![VALIDATION_LAYER.as_ptr()]
        } else {
            Vec::new()
        };
        let app_name = CString::new("vesper-ignis-vulkan").expect("no NUL");
        let app = vk::ApplicationInfo::default()
            .application_name(&app_name)
            .engine_name(&app_name)
            .api_version(vk::API_VERSION_1_3);
        let enables = [vk::ValidationFeatureEnableEXT::SYNCHRONIZATION_VALIDATION];
        let mut features =
            vk::ValidationFeaturesEXT::default().enabled_validation_features(&enables);
        let messages = Box::<Messages>::default();
        let mut messenger_info = vk::DebugUtilsMessengerCreateInfoEXT::default()
            .message_severity(
                vk::DebugUtilsMessageSeverityFlagsEXT::WARNING
                    | vk::DebugUtilsMessageSeverityFlagsEXT::ERROR,
            )
            .message_type(
                vk::DebugUtilsMessageTypeFlagsEXT::GENERAL
                    | vk::DebugUtilsMessageTypeFlagsEXT::VALIDATION
                    | vk::DebugUtilsMessageTypeFlagsEXT::PERFORMANCE,
            )
            .pfn_user_callback(Some(debug_callback))
            .user_data(&*messages as *const Messages as *mut c_void);
        let mut create = vk::InstanceCreateInfo::default()
            .application_info(&app)
            .enabled_layer_names(&layer_names)
            .enabled_extension_names(&ext_names);
        if sync_validation {
            create = create.push_next(&mut features);
        }
        if debug_utils {
            // Also reports errors during vkCreateInstance/vkDestroyInstance.
            create = create.push_next(&mut messenger_info);
        }
        // SAFETY: every pointer in `create` borrows locals that outlive the call.
        let instance = unsafe { entry.create_instance(&create, None) }
            .map_err(|e| unavailable(format!("vkCreateInstance: {e}")))?;
        // From here on the instance must be destroyed on every error path.
        let guard = InstanceGuard(Some(instance));
        let instance = guard.0.as_ref().expect("live");
        let debug = if debug_utils {
            let utils = ext::debug_utils::Instance::new(&entry, instance);
            // `messenger_info` was chained into the instance info (its pNext
            // now points at the validation features), so build a fresh one.
            let info = vk::DebugUtilsMessengerCreateInfoEXT {
                p_next: std::ptr::null(),
                ..messenger_info
            };
            // SAFETY: valid create info; destroyed in `Inner::drop`.
            let messenger = unsafe { utils.create_debug_utils_messenger(&info, None) }
                .map_err(|e| unavailable(format!("debug messenger: {e}")))?;
            Some((utils, messenger))
        } else {
            None
        };
        let debug_guard = DebugGuard(debug);
        let picked = pick_physical(instance, config)?;
        let (physical, props) = picked;
        // SAFETY: valid physical device from this instance.
        let family_props =
            unsafe { instance.get_physical_device_queue_family_properties(physical) };
        let find = |want: vk::QueueFlags, avoid: vk::QueueFlags| {
            family_props
                .iter()
                .enumerate()
                .find(|(_, f)| f.queue_flags.contains(want) && !f.queue_flags.intersects(avoid))
                .map(|(i, f)| Family {
                    index: i as u32,
                    count: f.queue_count.clamp(1, 4),
                })
        };
        let direct = find(
            vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE,
            vk::QueueFlags::empty(),
        )
        .ok_or_else(|| unavailable("no graphics+compute queue family"))?;
        let compute = find(vk::QueueFlags::COMPUTE, vk::QueueFlags::GRAPHICS).unwrap_or(direct);
        let copy = find(
            vk::QueueFlags::TRANSFER,
            vk::QueueFlags::GRAPHICS | vk::QueueFlags::COMPUTE,
        )
        .unwrap_or(compute);
        let mut sharing = vec![direct.index];
        for f in [compute.index, copy.index] {
            if !sharing.contains(&f) {
                sharing.push(f);
            }
        }
        let priorities = [1.0f32; 4];
        let queue_infos: Vec<_> = sharing
            .iter()
            .map(|index| {
                let fam = [direct, compute, copy]
                    .into_iter()
                    .find(|f| f.index == *index)
                    .expect("listed family");
                vk::DeviceQueueCreateInfo::default()
                    .queue_family_index(*index)
                    .queue_priorities(&priorities[..fam.count as usize])
            })
            .collect();
        let device_exts = [khr::push_descriptor::NAME.as_ptr()];
        let mut f12 = vk::PhysicalDeviceVulkan12Features::default().timeline_semaphore(true);
        let mut f13 = vk::PhysicalDeviceVulkan13Features::default()
            .synchronization2(true)
            .dynamic_rendering(true);
        let device_info = vk::DeviceCreateInfo::default()
            .queue_create_infos(&queue_infos)
            .enabled_extension_names(&device_exts)
            .push_next(&mut f12)
            .push_next(&mut f13);
        // SAFETY: features and extensions were checked in `pick_physical`.
        let device = unsafe { instance.create_device(physical, &device_info, None) }
            .map_err(|e| unavailable(format!("vkCreateDevice: {e}")))?;
        let queues = sharing
            .iter()
            .map(|index| {
                let fam = [direct, compute, copy]
                    .into_iter()
                    .find(|f| f.index == *index)
                    .expect("listed family");
                let list = (0..fam.count)
                    // SAFETY: the queue was requested at device creation.
                    .map(|q| unsafe { device.get_device_queue(*index, q) })
                    .collect();
                (*index, list)
            })
            .collect();
        // SAFETY: valid physical device.
        let memory = unsafe { instance.get_physical_device_memory_properties(physical) };
        let push = khr::push_descriptor::Device::new(instance, &device);
        let name = props
            .0
            .device_name_as_c_str()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        let info = DeviceInfo {
            name,
            driver_name: props
                .1
                .driver_name_as_c_str()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            driver_info: props
                .1
                .driver_info_as_c_str()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_default(),
            api_version: (
                vk::api_version_major(props.0.api_version),
                vk::api_version_minor(props.0.api_version),
                vk::api_version_patch(props.0.api_version),
            ),
            device_type: format!("{:?}", props.0.device_type),
            validation,
            sync_validation,
            families: sharing
                .iter()
                .map(|i| {
                    let f = &family_props[*i as usize];
                    (*i, family_flags(f.queue_flags), f.queue_count)
                })
                .collect(),
        };
        let mut guard = guard;
        let mut debug_guard = debug_guard;
        Ok(Device {
            inner: Arc::new(Inner {
                _entry: entry,
                instance: guard.0.take().expect("live"),
                debug: debug_guard.0.take(),
                messages,
                device,
                push,
                direct,
                compute,
                copy,
                sharing,
                queues,
                memory,
                limits: props.0.limits,
                info,
                lost: AtomicBool::new(false),
                submit_lock: Mutex::new(()),
            }),
        })
    }

    pub fn info(&self) -> &DeviceInfo {
        &self.inner.info
    }

    /// Validation messages (warnings and errors) received so far.
    pub fn validation_messages(&self) -> Vec<ValidationMessage> {
        self.inner
            .messages
            .list
            .lock()
            .map(|l| l.clone())
            .unwrap_or_default()
    }

    /// Validation errors received so far.
    pub fn validation_errors(&self) -> Vec<ValidationMessage> {
        self.validation_messages()
            .into_iter()
            .filter(|m| m.error)
            .collect()
    }

    /// Removes and returns every message received so far.
    pub fn take_validation_messages(&self) -> Vec<ValidationMessage> {
        self.inner
            .messages
            .list
            .lock()
            .map(|mut l| std::mem::take(&mut *l))
            .unwrap_or_default()
    }

    /// Fault injection: every later Vulkan call on this device reports
    /// `VK_ERROR_DEVICE_LOST`, exactly as a driver does after a real loss
    /// (a hang, reset or removal). The driver-side device is not affected;
    /// this exercises the error paths, not a driver's loss detection.
    pub fn inject_device_lost(&self) {
        self.inner.lost.store(true, Ordering::Release);
    }

    pub fn is_lost(&self) -> bool {
        self.inner.is_lost()
    }

    /// A host-visible, coherent, persistently mapped buffer usable as a
    /// storage buffer and copy source/destination. Contents start zeroed.
    pub fn create_buffer(&self, size: u64) -> Result<GpuBuffer, VulkanError> {
        self.inner.ensure_live("vkCreateBuffer")?;
        if size == 0 || size > (1 << 30) {
            return Err(VulkanError::InvalidDesc("buffer size must be in 1..=1 GiB"));
        }
        let d = &self.inner.device;
        let info = self
            .inner
            .sharing(vk::BufferCreateInfo::default().size(size).usage(
                vk::BufferUsageFlags::STORAGE_BUFFER
                    | vk::BufferUsageFlags::TRANSFER_SRC
                    | vk::BufferUsageFlags::TRANSFER_DST,
            ));
        // SAFETY: valid create info; destroyed on every error path below.
        let buffer = self
            .inner
            .check("vkCreateBuffer", unsafe { d.create_buffer(&info, None) })?;
        // SAFETY: `buffer` is a live buffer of this device.
        let req = unsafe { d.get_buffer_memory_requirements(buffer) };
        let destroy_buffer = || unsafe { d.destroy_buffer(buffer, None) };
        let Some(ty) = self.inner.memory_type(
            req.memory_type_bits,
            vk::MemoryPropertyFlags::HOST_VISIBLE | vk::MemoryPropertyFlags::HOST_COHERENT,
        ) else {
            destroy_buffer();
            return Err(VulkanError::NoMemoryType);
        };
        let alloc = vk::MemoryAllocateInfo::default()
            .allocation_size(req.size)
            .memory_type_index(ty);
        // SAFETY: valid allocate info.
        let memory = match self.inner.check("vkAllocateMemory", unsafe {
            d.allocate_memory(&alloc, None)
        }) {
            Ok(m) => m,
            Err(e) => {
                destroy_buffer();
                return Err(e);
            }
        };
        let free = || unsafe {
            d.free_memory(memory, None);
            d.destroy_buffer(buffer, None);
        };
        // SAFETY: unbound buffer, compatible memory at offset 0.
        if let Err(e) = self.inner.check("vkBindBufferMemory", unsafe {
            d.bind_buffer_memory(buffer, memory, 0)
        }) {
            free();
            return Err(e);
        }
        // SAFETY: host-visible memory, mapped once for the buffer's lifetime.
        let ptr = match self.inner.check("vkMapMemory", unsafe {
            d.map_memory(memory, 0, vk::WHOLE_SIZE, vk::MemoryMapFlags::empty())
        }) {
            Ok(p) => p as *mut u8,
            Err(e) => {
                free();
                return Err(e);
            }
        };
        // SAFETY: the mapping covers at least `size` bytes.
        unsafe { std::ptr::write_bytes(ptr, 0, size as usize) };
        Ok(GpuBuffer {
            inner: ManuallyDrop::new(self.inner.clone()),
            buffer,
            memory,
            size,
            ptr,
            live: true,
        })
    }

    /// A 2D RGBA8 texture usable as a color attachment, sampled image,
    /// storage image and copy source/destination, starting in `UNDEFINED`.
    pub fn create_texture(&self, desc: TextureInfo) -> Result<GpuTexture, VulkanError> {
        self.inner.ensure_live("vkCreateImage")?;
        let max = self.inner.limits.max_image_dimension2_d.min(16_384);
        if desc.width == 0 || desc.height == 0 || desc.width > max || desc.height > max {
            return Err(VulkanError::InvalidDesc("texture extent"));
        }
        let max_mips = 32 - desc.width.max(desc.height).leading_zeros();
        if desc.mip_levels == 0 || desc.mip_levels > max_mips {
            return Err(VulkanError::InvalidDesc("mip levels"));
        }
        if desc.array_layers == 0 || desc.array_layers > 64 {
            return Err(VulkanError::InvalidDesc("array layers"));
        }
        let d = &self.inner.device;
        let format = vk::Format::R8G8B8A8_UNORM;
        let info = self.inner.sharing(
            vk::ImageCreateInfo::default()
                .image_type(vk::ImageType::TYPE_2D)
                .format(format)
                .extent(vk::Extent3D {
                    width: desc.width,
                    height: desc.height,
                    depth: 1,
                })
                .mip_levels(desc.mip_levels)
                .array_layers(desc.array_layers)
                .samples(vk::SampleCountFlags::TYPE_1)
                .tiling(vk::ImageTiling::OPTIMAL)
                .usage(
                    vk::ImageUsageFlags::COLOR_ATTACHMENT
                        | vk::ImageUsageFlags::SAMPLED
                        | vk::ImageUsageFlags::STORAGE
                        | vk::ImageUsageFlags::TRANSFER_SRC
                        | vk::ImageUsageFlags::TRANSFER_DST,
                )
                .initial_layout(vk::ImageLayout::UNDEFINED),
        );
        // SAFETY: valid create info; destroyed on every error path below.
        let image = self
            .inner
            .check("vkCreateImage", unsafe { d.create_image(&info, None) })?;
        // SAFETY: live image.
        let req = unsafe { d.get_image_memory_requirements(image) };
        let ty = self
            .inner
            .memory_type(req.memory_type_bits, vk::MemoryPropertyFlags::DEVICE_LOCAL)
            .or_else(|| {
                self.inner
                    .memory_type(req.memory_type_bits, vk::MemoryPropertyFlags::empty())
            });
        let Some(ty) = ty else {
            // SAFETY: unused image.
            unsafe { d.destroy_image(image, None) };
            return Err(VulkanError::NoMemoryType);
        };
        let alloc = vk::MemoryAllocateInfo::default()
            .allocation_size(req.size)
            .memory_type_index(ty);
        // SAFETY: valid allocate info.
        let memory = match self.inner.check("vkAllocateMemory", unsafe {
            d.allocate_memory(&alloc, None)
        }) {
            Ok(m) => m,
            Err(e) => {
                // SAFETY: unused image.
                unsafe { d.destroy_image(image, None) };
                return Err(e);
            }
        };
        let free = || unsafe {
            d.free_memory(memory, None);
            d.destroy_image(image, None);
        };
        // SAFETY: unbound image, compatible memory.
        if let Err(e) = self.inner.check("vkBindImageMemory", unsafe {
            d.bind_image_memory(image, memory, 0)
        }) {
            free();
            return Err(e);
        }
        let view_info = vk::ImageViewCreateInfo::default()
            .image(image)
            .view_type(vk::ImageViewType::TYPE_2D)
            .format(format)
            .subresource_range(vk::ImageSubresourceRange {
                aspect_mask: vk::ImageAspectFlags::COLOR,
                base_mip_level: 0,
                level_count: 1,
                base_array_layer: 0,
                layer_count: 1,
            });
        // SAFETY: bound image, matching format.
        let view = match self.inner.check("vkCreateImageView", unsafe {
            d.create_image_view(&view_info, None)
        }) {
            Ok(v) => v,
            Err(e) => {
                free();
                return Err(e);
            }
        };
        let count = (desc.mip_levels * desc.array_layers) as usize;
        Ok(GpuTexture {
            inner: ManuallyDrop::new(self.inner.clone()),
            image,
            view,
            memory,
            desc,
            layouts: RefCell::new(vec![vk::ImageLayout::UNDEFINED; count]),
            live: true,
        })
    }

    /// A graphics pipeline for dynamic rendering into one RGBA8 attachment:
    /// no vertex buffers (positions come from `@builtin(vertex_index)`), a
    /// triangle list, no culling, and no descriptor bindings.
    pub fn create_graphics_pipeline(
        &self,
        vertex: &CompiledShader,
        fragment: &CompiledShader,
    ) -> Result<Pipeline, VulkanError> {
        if vertex.stage != Stage::Vertex || fragment.stage != Stage::Fragment {
            return Err(VulkanError::InvalidDesc("graphics pipeline stages"));
        }
        check_layout(vertex, &[])?;
        check_layout(fragment, &[])?;
        self.inner.ensure_live("vkCreateGraphicsPipelines")?;
        let d = &self.inner.device;
        let mut objects = PipelineObjects::new(self.inner.clone());
        objects.layout = self.inner.check("vkCreatePipelineLayout", unsafe {
            // SAFETY: empty layout.
            d.create_pipeline_layout(&vk::PipelineLayoutCreateInfo::default(), None)
        })?;
        let vs = objects.module(vertex)?;
        let fs = objects.module(fragment)?;
        let vs_name = CString::new(vertex.entry_point.as_str())
            .map_err(|_| VulkanError::InvalidDesc("entry point"))?;
        let fs_name = CString::new(fragment.entry_point.as_str())
            .map_err(|_| VulkanError::InvalidDesc("entry point"))?;
        let stages = [
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::VERTEX)
                .module(vs)
                .name(&vs_name),
            vk::PipelineShaderStageCreateInfo::default()
                .stage(vk::ShaderStageFlags::FRAGMENT)
                .module(fs)
                .name(&fs_name),
        ];
        let vertex_input = vk::PipelineVertexInputStateCreateInfo::default();
        let assembly = vk::PipelineInputAssemblyStateCreateInfo::default()
            .topology(vk::PrimitiveTopology::TRIANGLE_LIST);
        let viewport = vk::PipelineViewportStateCreateInfo::default()
            .viewport_count(1)
            .scissor_count(1);
        let raster = vk::PipelineRasterizationStateCreateInfo::default()
            .polygon_mode(vk::PolygonMode::FILL)
            .cull_mode(vk::CullModeFlags::NONE)
            .front_face(vk::FrontFace::COUNTER_CLOCKWISE)
            .line_width(1.0);
        let multisample = vk::PipelineMultisampleStateCreateInfo::default()
            .rasterization_samples(vk::SampleCountFlags::TYPE_1);
        let attachments = [vk::PipelineColorBlendAttachmentState::default()
            .color_write_mask(vk::ColorComponentFlags::RGBA)];
        let blend = vk::PipelineColorBlendStateCreateInfo::default().attachments(&attachments);
        let dynamic_states = [vk::DynamicState::VIEWPORT, vk::DynamicState::SCISSOR];
        let dynamic = vk::PipelineDynamicStateCreateInfo::default().dynamic_states(&dynamic_states);
        let formats = [vk::Format::R8G8B8A8_UNORM];
        let mut rendering =
            vk::PipelineRenderingCreateInfo::default().color_attachment_formats(&formats);
        let info = vk::GraphicsPipelineCreateInfo::default()
            .stages(&stages)
            .vertex_input_state(&vertex_input)
            .input_assembly_state(&assembly)
            .viewport_state(&viewport)
            .rasterization_state(&raster)
            .multisample_state(&multisample)
            .color_blend_state(&blend)
            .dynamic_state(&dynamic)
            .layout(objects.layout)
            .push_next(&mut rendering);
        // SAFETY: every referenced object is live; arrays borrow locals.
        let pipelines =
            unsafe { d.create_graphics_pipelines(vk::PipelineCache::null(), &[info], None) };
        objects.pipeline = self
            .inner
            .check("vkCreateGraphicsPipelines", pipelines.map_err(|(_, e)| e))?[0];
        Ok(objects.finish(PipelineKind::Graphics))
    }

    /// A compute pipeline whose `@group(0)` bindings are exactly `bindings`,
    /// bound with push descriptors.
    pub fn create_compute_pipeline(
        &self,
        compute: &CompiledShader,
        bindings: &[BindingKind],
    ) -> Result<Pipeline, VulkanError> {
        if compute.stage != Stage::Compute {
            return Err(VulkanError::InvalidDesc("compute pipeline stage"));
        }
        if bindings.len() > crate::shader::MAX_BINDINGS as usize {
            return Err(VulkanError::Limit("bindings"));
        }
        check_layout(compute, bindings)?;
        self.inner.ensure_live("vkCreateComputePipelines")?;
        let d = &self.inner.device;
        let mut objects = PipelineObjects::new(self.inner.clone());
        let layout_bindings: Vec<_> = bindings
            .iter()
            .enumerate()
            .map(|(i, kind)| {
                vk::DescriptorSetLayoutBinding::default()
                    .binding(i as u32)
                    .descriptor_type(descriptor_type(*kind))
                    .descriptor_count(1)
                    .stage_flags(vk::ShaderStageFlags::COMPUTE)
            })
            .collect();
        let set_info = vk::DescriptorSetLayoutCreateInfo::default()
            .flags(vk::DescriptorSetLayoutCreateFlags::PUSH_DESCRIPTOR_KHR)
            .bindings(&layout_bindings);
        objects.set_layout = self.inner.check("vkCreateDescriptorSetLayout", unsafe {
            // SAFETY: valid create info.
            d.create_descriptor_set_layout(&set_info, None)
        })?;
        let set_layouts = [objects.set_layout];
        let layout_info = vk::PipelineLayoutCreateInfo::default().set_layouts(&set_layouts);
        objects.layout = self.inner.check("vkCreatePipelineLayout", unsafe {
            // SAFETY: valid create info.
            d.create_pipeline_layout(&layout_info, None)
        })?;
        let module = objects.module(compute)?;
        let name = CString::new(compute.entry_point.as_str())
            .map_err(|_| VulkanError::InvalidDesc("entry point"))?;
        let stage = vk::PipelineShaderStageCreateInfo::default()
            .stage(vk::ShaderStageFlags::COMPUTE)
            .module(module)
            .name(&name);
        let info = vk::ComputePipelineCreateInfo::default()
            .stage(stage)
            .layout(objects.layout);
        // SAFETY: live module and layout.
        let pipelines =
            unsafe { d.create_compute_pipelines(vk::PipelineCache::null(), &[info], None) };
        objects.pipeline = self
            .inner
            .check("vkCreateComputePipelines", pipelines.map_err(|(_, e)| e))?[0];
        Ok(objects.finish(PipelineKind::Compute(bindings.to_vec())))
    }

    /// A command allocator (`VkCommandPool`) for queues of `kind`.
    pub fn create_command_allocator(
        &self,
        kind: QueueKind,
    ) -> Result<CommandAllocator, VulkanError> {
        self.inner.ensure_live("vkCreateCommandPool")?;
        let family = self.inner.family(kind).index;
        let info = vk::CommandPoolCreateInfo::default()
            .flags(vk::CommandPoolCreateFlags::TRANSIENT)
            .queue_family_index(family);
        let pool = self.inner.check("vkCreateCommandPool", unsafe {
            // SAFETY: valid create info.
            self.inner.device.create_command_pool(&info, None)
        })?;
        Ok(CommandAllocator {
            inner: ManuallyDrop::new(self.inner.clone()),
            pool,
            buffers: RefCell::new(Vec::new()),
            family,
            kind,
            live: true,
        })
    }
}

struct InstanceGuard(Option<ash::Instance>);
impl Drop for InstanceGuard {
    fn drop(&mut self) {
        if let Some(instance) = self.0.take() {
            // SAFETY: only reached on error paths before `Inner` owns it.
            unsafe { instance.destroy_instance(None) };
        }
    }
}
struct DebugGuard(Option<(ext::debug_utils::Instance, vk::DebugUtilsMessengerEXT)>);
impl Drop for DebugGuard {
    fn drop(&mut self) {
        if let Some((utils, messenger)) = self.0.take() {
            // SAFETY: only reached on error paths, before the instance guard.
            unsafe { utils.destroy_debug_utils_messenger(messenger, None) };
        }
    }
}

type Props = (
    vk::PhysicalDeviceProperties,
    vk::PhysicalDeviceDriverProperties<'static>,
);

fn pick_physical(
    instance: &ash::Instance,
    config: &DeviceConfig,
) -> Result<(vk::PhysicalDevice, Props), VulkanError> {
    // SAFETY: valid instance.
    let devices = unsafe { instance.enumerate_physical_devices() }
        .map_err(|e| unavailable(format!("vkEnumeratePhysicalDevices: {e}")))?;
    let mut reasons = Vec::new();
    for physical in devices {
        let mut driver = vk::PhysicalDeviceDriverProperties::default();
        let mut props2 = vk::PhysicalDeviceProperties2::default().push_next(&mut driver);
        // SAFETY: valid physical device; the chain borrows locals.
        unsafe { instance.get_physical_device_properties2(physical, &mut props2) };
        let props = props2.properties;
        let name = props
            .device_name_as_c_str()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_default();
        if let Some(want) = &config.device_name
            && !name.to_lowercase().contains(&want.to_lowercase())
        {
            reasons.push(format!("{name}: not {want:?}"));
            continue;
        }
        if props.api_version < vk::API_VERSION_1_3 {
            reasons.push(format!("{name}: below Vulkan 1.3"));
            continue;
        }
        let mut f12 = vk::PhysicalDeviceVulkan12Features::default();
        let mut f13 = vk::PhysicalDeviceVulkan13Features::default();
        let mut f2 = vk::PhysicalDeviceFeatures2::default()
            .push_next(&mut f12)
            .push_next(&mut f13);
        // SAFETY: as above.
        unsafe { instance.get_physical_device_features2(physical, &mut f2) };
        if f12.timeline_semaphore == vk::FALSE
            || f13.synchronization2 == vk::FALSE
            || f13.dynamic_rendering == vk::FALSE
        {
            reasons.push(format!("{name}: missing a required feature"));
            continue;
        }
        // SAFETY: as above.
        let exts =
            unsafe { instance.enumerate_device_extension_properties(physical) }.unwrap_or_default();
        if !exts
            .iter()
            .any(|e| e.extension_name_as_c_str().ok() == Some(khr::push_descriptor::NAME))
        {
            reasons.push(format!("{name}: no VK_KHR_push_descriptor"));
            continue;
        }
        // Detach the driver properties from the stack chain.
        let driver_out = vk::PhysicalDeviceDriverProperties {
            driver_id: driver.driver_id,
            driver_name: driver.driver_name,
            driver_info: driver.driver_info,
            conformance_version: driver.conformance_version,
            ..Default::default()
        };
        return Ok((physical, (props, driver_out)));
    }
    Err(unavailable(if reasons.is_empty() {
        "no physical devices".to_owned()
    } else {
        reasons.join("; ")
    }))
}

pub(crate) fn descriptor_type(kind: BindingKind) -> vk::DescriptorType {
    match kind {
        BindingKind::SampledTexture => vk::DescriptorType::SAMPLED_IMAGE,
        BindingKind::StorageTexture => vk::DescriptorType::STORAGE_IMAGE,
        BindingKind::StorageBuffer => vk::DescriptorType::STORAGE_BUFFER,
    }
}

/// Parameters of [`Device::create_texture`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextureInfo {
    pub width: u32,
    pub height: u32,
    pub mip_levels: u32,
    pub array_layers: u32,
}

impl TextureInfo {
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            width,
            height,
            mip_levels: 1,
            array_layers: 1,
        }
    }
}

/// Owned host-visible buffer. See the module docs for the ownership rules.
#[must_use = "a GpuBuffer must be released with destroy()"]
pub struct GpuBuffer {
    pub(crate) inner: ManuallyDrop<Arc<Inner>>,
    pub(crate) buffer: vk::Buffer,
    memory: vk::DeviceMemory,
    size: u64,
    ptr: *mut u8,
    live: bool,
}

impl std::fmt::Debug for GpuBuffer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuBuffer")
            .field("size", &self.size)
            .finish_non_exhaustive()
    }
}

impl GpuBuffer {
    pub fn size(&self) -> u64 {
        self.size
    }
    /// The buffer's bytes. A `GpuBuffer` the caller can reach is never in
    /// flight: submitted buffers are owned by the lifecycle runtime until
    /// their frame retires, and the backend ends every queue stream with a
    /// device-to-host memory barrier before its terminal signal.
    pub fn read(&self) -> Vec<u8> {
        let mut out = vec![0; self.size as usize];
        // SAFETY: the mapping covers `size` bytes and no GPU work is pending
        // (see above); coherent memory needs no invalidation.
        unsafe { std::ptr::copy_nonoverlapping(self.ptr, out.as_mut_ptr(), out.len()) };
        out
    }
    /// Copies `bytes` to `offset`. Host writes are visible to work submitted
    /// afterwards (queue submission is a host-to-device domain operation).
    pub fn write(&mut self, offset: u64, bytes: &[u8]) -> Result<(), VulkanError> {
        let end = offset
            .checked_add(bytes.len() as u64)
            .filter(|end| *end <= self.size)
            .ok_or(VulkanError::OutOfBounds("buffer write"))?;
        let _ = end;
        // SAFETY: bounds checked; `&mut self` excludes other host access and
        // the buffer is not in flight.
        unsafe {
            std::ptr::copy_nonoverlapping(
                bytes.as_ptr(),
                self.ptr.add(offset as usize),
                bytes.len(),
            )
        };
        Ok(())
    }
    pub fn destroy(mut self) {
        // SAFETY: not in flight (see `read`); the handles are owned.
        unsafe {
            let d = &self.inner.device;
            d.unmap_memory(self.memory);
            d.destroy_buffer(self.buffer, None);
            d.free_memory(self.memory, None);
            ManuallyDrop::drop(&mut self.inner);
        }
        self.live = false;
    }
}

impl Drop for GpuBuffer {
    fn drop(&mut self) {
        if self.live {
            count_leak();
        }
    }
}

/// Owned RGBA8 texture. Its tracked per-subresource Vulkan layout is the
/// layout after all work submitted so far.
#[must_use = "a GpuTexture must be released with destroy()"]
pub struct GpuTexture {
    pub(crate) inner: ManuallyDrop<Arc<Inner>>,
    pub(crate) image: vk::Image,
    pub(crate) view: vk::ImageView,
    memory: vk::DeviceMemory,
    desc: TextureInfo,
    /// Indexed `mip + mips * layer`.
    pub(crate) layouts: RefCell<Vec<vk::ImageLayout>>,
    live: bool,
}

impl std::fmt::Debug for GpuTexture {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GpuTexture")
            .field("desc", &self.desc)
            .finish_non_exhaustive()
    }
}

impl GpuTexture {
    pub fn info(&self) -> TextureInfo {
        self.desc
    }
    /// Tracked Vulkan layout of one subresource.
    pub fn layout(&self, mip: u32, layer: u32) -> Option<vk::ImageLayout> {
        if mip >= self.desc.mip_levels || layer >= self.desc.array_layers {
            return None;
        }
        self.layouts
            .borrow()
            .get((mip + self.desc.mip_levels * layer) as usize)
            .copied()
    }
    pub fn destroy(mut self) {
        // SAFETY: not in flight (owned by the caller, see `GpuBuffer::read`).
        unsafe {
            let d = &self.inner.device;
            d.destroy_image_view(self.view, None);
            d.destroy_image(self.image, None);
            d.free_memory(self.memory, None);
            ManuallyDrop::drop(&mut self.inner);
        }
        self.live = false;
    }
}

impl Drop for GpuTexture {
    fn drop(&mut self) {
        if self.live {
            count_leak();
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum PipelineKind {
    Graphics,
    Compute(Vec<BindingKind>),
}

struct PipelineObjects {
    inner: Arc<Inner>,
    modules: Vec<vk::ShaderModule>,
    set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
    done: bool,
}

impl PipelineObjects {
    fn new(inner: Arc<Inner>) -> Self {
        Self {
            inner,
            modules: Vec::new(),
            set_layout: vk::DescriptorSetLayout::null(),
            layout: vk::PipelineLayout::null(),
            pipeline: vk::Pipeline::null(),
            done: false,
        }
    }
    fn module(&mut self, shader: &CompiledShader) -> Result<vk::ShaderModule, VulkanError> {
        let info = vk::ShaderModuleCreateInfo::default().code(&shader.spirv);
        let module = self.inner.check("vkCreateShaderModule", unsafe {
            // SAFETY: naga emitted validated SPIR-V words.
            self.inner.device.create_shader_module(&info, None)
        })?;
        self.modules.push(module);
        Ok(module)
    }
    fn finish(mut self, kind: PipelineKind) -> Pipeline {
        self.done = true;
        Pipeline {
            inner: ManuallyDrop::new(self.inner.clone()),
            modules: std::mem::take(&mut self.modules),
            set_layout: self.set_layout,
            layout: self.layout,
            pipeline: self.pipeline,
            kind,
            live: true,
        }
    }
}

impl Drop for PipelineObjects {
    fn drop(&mut self) {
        if self.done {
            return;
        }
        // SAFETY: error path; nothing was submitted with these objects.
        unsafe {
            destroy_pipeline_objects(
                &self.inner,
                &self.modules,
                self.set_layout,
                self.layout,
                self.pipeline,
            )
        };
    }
}

unsafe fn destroy_pipeline_objects(
    inner: &Inner,
    modules: &[vk::ShaderModule],
    set_layout: vk::DescriptorSetLayout,
    layout: vk::PipelineLayout,
    pipeline: vk::Pipeline,
) {
    let d = &inner.device;
    // SAFETY: the caller guarantees the objects are unused; null handles are
    // ignored by the destroy calls.
    unsafe {
        d.destroy_pipeline(pipeline, None);
        d.destroy_pipeline_layout(layout, None);
        d.destroy_descriptor_set_layout(set_layout, None);
        for m in modules {
            d.destroy_shader_module(*m, None);
        }
    }
}

/// Owned graphics or compute pipeline.
#[must_use = "a Pipeline must be released with destroy()"]
pub struct Pipeline {
    pub(crate) inner: ManuallyDrop<Arc<Inner>>,
    modules: Vec<vk::ShaderModule>,
    set_layout: vk::DescriptorSetLayout,
    pub(crate) layout: vk::PipelineLayout,
    pub(crate) pipeline: vk::Pipeline,
    pub(crate) kind: PipelineKind,
    live: bool,
}

impl std::fmt::Debug for Pipeline {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Pipeline")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl Pipeline {
    pub fn is_compute(&self) -> bool {
        matches!(self.kind, PipelineKind::Compute(_))
    }
    pub fn destroy(mut self) {
        // SAFETY: not in flight (owned by the caller).
        unsafe {
            destroy_pipeline_objects(
                &self.inner,
                &self.modules,
                self.set_layout,
                self.layout,
                self.pipeline,
            );
            ManuallyDrop::drop(&mut self.inner);
        }
        self.live = false;
    }
}

impl Drop for Pipeline {
    fn drop(&mut self) {
        if self.live {
            count_leak();
        }
    }
}

/// Owned `VkCommandPool`, the backend's `Allocator` payload. The backend
/// resets it at the start of each submission it is used for; the lifecycle
/// runtime returns it only after that frame's original fences completed.
#[must_use = "a CommandAllocator must be released with destroy()"]
pub struct CommandAllocator {
    pub(crate) inner: ManuallyDrop<Arc<Inner>>,
    pub(crate) pool: vk::CommandPool,
    /// Command buffers allocated from the pool, reused after each reset.
    pub(crate) buffers: RefCell<Vec<vk::CommandBuffer>>,
    pub(crate) family: u32,
    kind: QueueKind,
    live: bool,
}

impl std::fmt::Debug for CommandAllocator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CommandAllocator")
            .field("kind", &self.kind)
            .finish_non_exhaustive()
    }
}

impl CommandAllocator {
    pub fn kind(&self) -> QueueKind {
        self.kind
    }
    pub fn destroy(mut self) {
        // SAFETY: not in flight (owned by the caller).
        unsafe {
            self.inner.device.destroy_command_pool(self.pool, None);
            ManuallyDrop::drop(&mut self.inner);
        }
        self.live = false;
    }
}

impl Drop for CommandAllocator {
    fn drop(&mut self) {
        if self.live {
            count_leak();
        }
    }
}
