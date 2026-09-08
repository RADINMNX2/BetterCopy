use std::path::Path;
use windows::core::PCWSTR;
use windows::Win32::Foundation::{CloseHandle, HANDLE, INVALID_HANDLE_VALUE};
use windows::Win32::Storage::FileSystem::{
    CreateFileW, GetDriveTypeW, GetVolumePathNameW, FILE_SHARE_READ, FILE_SHARE_WRITE,
    OPEN_EXISTING, FILE_ATTRIBUTE_NORMAL,
};
use windows::Win32::System::IO::DeviceIoControl;

// Local constants for drive type
const DRIVE_REMOTE: u32 = 4;

// IOCTL and standard Windows storage API constants
const IOCTL_STORAGE_GET_DEVICE_NUMBER: u32 = 0x002d1080;
const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x002d1400;

const STORAGE_DEVICE_PROPERTY: i32 = 0;
const STORAGE_DEVICE_SEEK_PENALTY_PROPERTY: i32 = 7;
const PROPERTY_STANDARD_QUERY: i32 = 0;

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct STORAGE_DEVICE_NUMBER {
    pub device_type: u32,
    pub device_number: u32,
    pub partition_number: u32,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct STORAGE_PROPERTY_QUERY {
    pub property_id: i32,
    pub query_type: i32,
    pub additional_parameters: [u8; 1],
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct DEVICE_SEEK_PENALTY_DESCRIPTOR {
    pub version: u32,
    pub size: u32,
    pub incurs_seek_penalty: u8,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct STORAGE_DEVICE_DESCRIPTOR {
    pub version: u32,
    pub size: u32,
    pub device_type: u8,
    pub device_type_modifier: u8,
    pub removable_media: u8,
    pub command_queueing: u8,
    pub vendor_id_offset: u32,
    pub product_id_offset: u32,
    pub product_revision_offset: u32,
    pub serial_number_offset: u32,
    pub bus_type: u32,
    pub raw_properties_length: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeviceClass {
    SsdNvme,      // Internal NVMe or UASP USB
    SataSsd,      // Internal SATA SSD (shallower queue depth benefits are smaller)
    Unknown,      // Device didn't report clearly
    BotUsb,       // BOT USB or standard flash drive
    Hdd,          // Seek penalty detected
}

impl DeviceClass {
    pub fn default_concurrency(&self) -> usize {
        match self {
            DeviceClass::SsdNvme => 16,
            DeviceClass::SataSsd => 8,
            DeviceClass::Unknown => 16,
            DeviceClass::BotUsb => 2,
            DeviceClass::Hdd => 1,
        }
    }
}

#[derive(Debug, Clone)]
pub struct DeviceProfile {
    pub class: DeviceClass,
    pub physical_device_number: Option<u32>,
    pub concurrency: usize,
    pub is_remote: bool,
    pub description: String,
}

/// Helper to get the volume path for a given file/dir path.
pub fn get_volume_path(path: &Path) -> Option<Vec<u16>> {
    let path_str = path.to_string_lossy();
    let wide_path: Vec<u16> = path_str.encode_utf16().chain(std::iter::once(0)).collect();
    let mut volume_path = vec![0u16; 512];
    unsafe {
        let res = GetVolumePathNameW(
            PCWSTR(wide_path.as_ptr()),
            volume_path.as_mut_slice(),
        );
        if res.is_ok() {
            if let Some(pos) = volume_path.iter().position(|&x| x == 0) {
                volume_path.truncate(pos);
            }
            Some(volume_path)
        } else {
            None
        }
    }
}

/// Strip trailing backslash and format properly for opening volume (e.g. C:\ -> \\.\C:)
fn make_volume_device_path(volume_path: &[u16]) -> Vec<u16> {
    let mut s = String::from_utf16_lossy(volume_path);
    if s.ends_with('\\') {
        s.pop();
    }
    if s.len() == 2 && s.chars().next().map(|c| c.is_ascii_alphabetic()).unwrap_or(false) && s.ends_with(':') {
        s = format!("\\\\.\\{}", s);
    }
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Opens a handle to the volume. Passing 0 for desired access allows querying device IOCTLs
/// without admin privileges.
fn open_volume(device_path: &[u16]) -> Option<HANDLE> {
    unsafe {
        let handle = CreateFileW(
            PCWSTR(device_path.as_ptr()),
            0, // No specific access needed for querying device properties
            FILE_SHARE_READ | FILE_SHARE_WRITE,
            None,
            OPEN_EXISTING,
            FILE_ATTRIBUTE_NORMAL,
            None,
        );
        if let Ok(h) = handle {
            if h == INVALID_HANDLE_VALUE {
                None
            } else {
                Some(h)
            }
        } else {
            None
        }
    }
}

pub fn profile_device(path: &Path) -> DeviceProfile {
    let volume_path_wide = match get_volume_path(path) {
        Some(vp) => vp,
        None => {
            return DeviceProfile {
                class: DeviceClass::Unknown,
                physical_device_number: None,
                concurrency: DeviceClass::Unknown.default_concurrency(),
                is_remote: false,
                description: "Unknown volume path".to_string(),
            };
        }
    };

    // Ensure we have a null-terminated wide string for GetDriveTypeW
    let mut volume_path_with_null = volume_path_wide.clone();
    if !volume_path_with_null.ends_with(&[92]) && !volume_path_with_null.ends_with(&[0]) {
        volume_path_with_null.push(92); // Add trailing backslash ('\')
    }
    volume_path_with_null.push(0);

    let drive_type = unsafe { GetDriveTypeW(PCWSTR(volume_path_with_null.as_ptr())) };
    if drive_type == DRIVE_REMOTE {
        return DeviceProfile {
            class: DeviceClass::Unknown,
            physical_device_number: None,
            concurrency: 2, // Network copies defaults to low concurrency to prevent network congestion
            is_remote: true,
            description: "Remote network drive".to_string(),
        };
    }

    let device_path = make_volume_device_path(&volume_path_wide);
    let handle = match open_volume(&device_path) {
        Some(h) => h,
        None => {
            // Fallback for inaccessible local volumes
            return DeviceProfile {
                class: DeviceClass::Unknown,
                physical_device_number: None,
                concurrency: DeviceClass::Unknown.default_concurrency(),
                is_remote: false,
                description: format!("Local drive (failed to open volume {})", String::from_utf16_lossy(&device_path)),
            };
        }
    };

    // 1. Get physical device number
    let mut device_number: Option<u32> = None;
    let mut sdn = STORAGE_DEVICE_NUMBER {
        device_type: 0,
        device_number: 0,
        partition_number: 0,
    };
    let mut bytes_returned = 0u32;
    unsafe {
        let res = DeviceIoControl(
            handle,
            IOCTL_STORAGE_GET_DEVICE_NUMBER,
            None,
            0,
            Some(&mut sdn as *mut _ as *mut std::ffi::c_void),
            std::mem::size_of::<STORAGE_DEVICE_NUMBER>() as u32,
            Some(&mut bytes_returned),
            None,
        );
        if res.is_ok() {
            device_number = Some(sdn.device_number);
        }
    }

    // 2. Query seek penalty (HDD vs SSD)
    let mut incurs_seek_penalty = None;
    let query_seek = STORAGE_PROPERTY_QUERY {
        property_id: STORAGE_DEVICE_SEEK_PENALTY_PROPERTY,
        query_type: PROPERTY_STANDARD_QUERY,
        additional_parameters: [0],
    };
    let mut seek_penalty_desc = DEVICE_SEEK_PENALTY_DESCRIPTOR {
        version: 0,
        size: 0,
        incurs_seek_penalty: 0,
    };
    unsafe {
        let res = DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(&query_seek as *const _ as *const std::ffi::c_void),
            std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(&mut seek_penalty_desc as *mut _ as *mut std::ffi::c_void),
            std::mem::size_of::<DEVICE_SEEK_PENALTY_DESCRIPTOR>() as u32,
            Some(&mut bytes_returned),
            None,
        );
        if res.is_ok() {
            incurs_seek_penalty = Some(seek_penalty_desc.incurs_seek_penalty != 0);
        }
    }

    // 3. Query bus type and queuing (UASP vs BOT)
    let mut bus_type = None;
    let mut command_queuing = None;
    let query_dev = STORAGE_PROPERTY_QUERY {
        property_id: STORAGE_DEVICE_PROPERTY,
        query_type: PROPERTY_STANDARD_QUERY,
        additional_parameters: [0],
    };
    let mut dev_desc = STORAGE_DEVICE_DESCRIPTOR {
        version: 0,
        size: 0,
        device_type: 0,
        device_type_modifier: 0,
        removable_media: 0,
        command_queueing: 0,
        vendor_id_offset: 0,
        product_id_offset: 0,
        product_revision_offset: 0,
        serial_number_offset: 0,
        bus_type: 0,
        raw_properties_length: 0,
    };
    unsafe {
        let res = DeviceIoControl(
            handle,
            IOCTL_STORAGE_QUERY_PROPERTY,
            Some(&query_dev as *const _ as *const std::ffi::c_void),
            std::mem::size_of::<STORAGE_PROPERTY_QUERY>() as u32,
            Some(&mut dev_desc as *mut _ as *mut std::ffi::c_void),
            std::mem::size_of::<STORAGE_DEVICE_DESCRIPTOR>() as u32,
            Some(&mut bytes_returned),
            None,
        );
        if res.is_ok() {
            bus_type = Some(dev_desc.bus_type);
            command_queuing = Some(dev_desc.command_queueing != 0);
        }
    }

    unsafe {
        let _ = CloseHandle(handle);
    }

    // Determine device class and description
    let class;
    let description;

    match incurs_seek_penalty {
        Some(true) => {
            class = DeviceClass::Hdd;
            description = "Spinning HDD (Seek penalty detected)".to_string();
        }
        Some(false) => {
            // Seek penalty is false -> SSD. Let's see if USB, and if UASP or BOT.
            if let Some(bus) = bus_type {
                if bus == 7 { // BusTypeUsb
                    if command_queuing == Some(true) {
                        class = DeviceClass::SsdNvme;
                        description = "USB SSD (UASP command queuing enabled)".to_string();
                    } else {
                        class = DeviceClass::BotUsb;
                        description = "USB Storage (BOT, command queuing disabled)".to_string();
                    }
                } else if bus == 17 {
                    class = DeviceClass::SsdNvme;
                    description = "Internal NVMe SSD".to_string();
                } else if bus == 11 {
                    class = DeviceClass::SataSsd;
                    description = "Internal SATA SSD".to_string();
                } else {
                    class = DeviceClass::SsdNvme;
                    description = format!("SSD (Bus type {})", bus);
                }
            } else {
                class = DeviceClass::SsdNvme;
                description = "SSD (Seek penalty false, unknown bus)".to_string();
            }
        }
        None => {
            // Seek penalty could not be determined. Let's see if we have bus type.
            if let Some(bus) = bus_type {
                if bus == 7 {
                    if command_queuing == Some(true) {
                        class = DeviceClass::SsdNvme;
                        description = "USB SSD (UASP, seek penalty unknown)".to_string();
                    } else {
                        class = DeviceClass::BotUsb;
                        description = "USB Storage (BOT, seek penalty unknown)".to_string();
                    }
                } else {
                    class = DeviceClass::Unknown;
                    description = format!("Local Drive (Bus type {}, seek penalty unknown)", bus);
                }
            } else {
                class = DeviceClass::Unknown;
                description = "Local Drive (Seek penalty and bus type unknown)".to_string();
            }
        }
    }

    DeviceProfile {
        class,
        physical_device_number: device_number,
        concurrency: class.default_concurrency(),
        is_remote: false,
        description,
    }
}

/// Helper to get the free disk space of a volume (returns bytes)
pub fn get_disk_free_space(volume_path: &[u16]) -> Option<u64> {
    unsafe {
        let mut free_bytes = 0u64;
        let res = windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(
            PCWSTR(volume_path.as_ptr()),
            Some(&mut free_bytes),
            None,
            None,
        );
        if res.is_ok() {
            Some(free_bytes)
        } else {
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_profile_device() {
        let path = Path::new("C:\\");
        let profile = profile_device(path);
        println!("profile: {:?}", profile);
        assert!(!profile.description.is_empty());
    }
}

