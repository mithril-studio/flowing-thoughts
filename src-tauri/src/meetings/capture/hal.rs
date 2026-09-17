//! OWNER: WP5 (capture). Core Audio HAL property helpers. macOS only.
//!
//! Nothing here may run on the real-time audio thread: property reads take
//! HAL locks and can block for seconds while a Bluetooth device goes away.

use std::ffi::c_void;
use std::mem::MaybeUninit;
use std::ptr::NonNull;

use objc2_core_audio::{
    kAudioDevicePropertyDataSource, kAudioDevicePropertyDeviceIsRunningSomewhere,
    kAudioDevicePropertyDeviceUID,
    kAudioDevicePropertyNominalSampleRate, kAudioDevicePropertyTransportType,
    kAudioHardwarePropertyDefaultInputDevice, kAudioHardwarePropertyDefaultOutputDevice,
    kAudioHardwarePropertyDevices, kAudioHardwarePropertyProcessObjectList,
    kAudioHardwarePropertyTranslatePIDToProcessObject,
    kAudioObjectPropertyElementMain, kAudioObjectPropertyName, kAudioObjectPropertyScopeGlobal,
    kAudioObjectPropertyScopeOutput, kAudioObjectSystemObject, kAudioProcessPropertyIsRunningOutput,
    AudioObjectGetPropertyData, AudioObjectGetPropertyDataSize,
    AudioObjectID, AudioObjectPropertyAddress,
};
use objc2_core_foundation::{CFRetained, CFString};

pub type OSStatus = i32;

pub const SYSTEM_OBJECT: AudioObjectID = kAudioObjectSystemObject as AudioObjectID;
/// `kAudioObjectUnknown`: what the HAL reports when there is no default device.
pub const UNKNOWN_OBJECT: AudioObjectID = 0;

/// An OSStatus as its four-char code when it is one (`'who?'`, `'!dev'`).
pub fn status_str(status: OSStatus) -> String {
    let bytes = (status as u32).to_be_bytes();
    if bytes.iter().all(|c| c.is_ascii_graphic() || *c == b' ') {
        format!("{status} ('{}')", String::from_utf8_lossy(&bytes))
    } else {
        format!("{status}")
    }
}

pub fn address(selector: u32) -> AudioObjectPropertyAddress {
    scoped_address(selector, kAudioObjectPropertyScopeGlobal)
}

pub fn scoped_address(selector: u32, scope: u32) -> AudioObjectPropertyAddress {
    AudioObjectPropertyAddress {
        mSelector: selector,
        mScope: scope,
        mElement: kAudioObjectPropertyElementMain,
    }
}

/// Reads a fixed-size property.
///
/// # Safety
/// `T` must be the property's data type.
pub unsafe fn get_prop_at<T>(
    object: AudioObjectID,
    addr: AudioObjectPropertyAddress,
) -> Result<T, OSStatus> {
    let mut value = MaybeUninit::<T>::zeroed();
    let mut size = std::mem::size_of::<T>() as u32;
    let status = AudioObjectGetPropertyData(
        object,
        NonNull::from(&addr),
        0,
        std::ptr::null(),
        NonNull::from(&mut size),
        NonNull::new_unchecked(value.as_mut_ptr()).cast(),
    );
    if status == 0 {
        Ok(value.assume_init())
    } else {
        Err(status)
    }
}

/// # Safety
/// `T` must be the property's data type.
pub unsafe fn get_prop<T>(object: AudioObjectID, selector: u32) -> Result<T, OSStatus> {
    get_prop_at(object, address(selector))
}

/// Reads an array property of `AudioObjectID`s.
fn get_object_list(object: AudioObjectID, selector: u32) -> Result<Vec<AudioObjectID>, OSStatus> {
    let addr = address(selector);
    let mut size = 0u32;
    // SAFETY: valid address and out-pointer.
    let status = unsafe {
        AudioObjectGetPropertyDataSize(
            object,
            NonNull::from(&addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
        )
    };
    if status != 0 {
        return Err(status);
    }
    let count = size as usize / std::mem::size_of::<AudioObjectID>();
    // Room to spare: the list can grow between the two calls.
    let mut ids = vec![0 as AudioObjectID; count + 8];
    let mut size = (ids.len() * std::mem::size_of::<AudioObjectID>()) as u32;
    // SAFETY: `ids` holds `size` bytes; the HAL writes back how many it used.
    let status = unsafe {
        AudioObjectGetPropertyData(
            object,
            NonNull::from(&addr),
            0,
            std::ptr::null(),
            NonNull::from(&mut size),
            NonNull::new_unchecked(ids.as_mut_ptr()).cast::<c_void>(),
        )
    };
    if status != 0 {
        return Err(status);
    }
    ids.truncate(size as usize / std::mem::size_of::<AudioObjectID>());
    Ok(ids)
}

fn get_string(object: AudioObjectID, selector: u32) -> Result<CFRetained<CFString>, OSStatus> {
    // SAFETY: both string properties used here are `CFStringRef`.
    let raw: *const CFString = unsafe { get_prop(object, selector)? };
    match NonNull::new(raw as *mut CFString) {
        // The HAL hands back a +1 reference.
        Some(ptr) => Ok(unsafe { CFRetained::from_raw(ptr) }),
        None => Err(-1),
    }
}

pub fn device_uid(device: AudioObjectID) -> Result<CFRetained<CFString>, OSStatus> {
    get_string(device, kAudioDevicePropertyDeviceUID)
}

pub fn device_name(device: AudioObjectID) -> Option<String> {
    get_string(device, kAudioObjectPropertyName).ok().map(|s| s.to_string())
}

pub fn nominal_sample_rate(device: AudioObjectID) -> Option<f64> {
    // SAFETY: the property is a Float64.
    unsafe { get_prop::<f64>(device, kAudioDevicePropertyNominalSampleRate).ok() }
}

fn default_device(selector: u32) -> Result<AudioObjectID, String> {
    // SAFETY: both default-device properties are an AudioObjectID.
    match unsafe { get_prop::<AudioObjectID>(SYSTEM_OBJECT, selector) } {
        Ok(UNKNOWN_OBJECT) => Err("there is none".to_string()),
        Ok(device) => Ok(device),
        Err(status) => Err(status_str(status)),
    }
}

pub fn default_output_device() -> Result<AudioObjectID, String> {
    default_device(kAudioHardwarePropertyDefaultOutputDevice)
        .map_err(|e| format!("No default output device: {e}"))
}

pub fn default_input_device() -> Result<AudioObjectID, String> {
    default_device(kAudioHardwarePropertyDefaultInputDevice)
        .map_err(|e| format!("No default input device: {e}"))
}

pub fn all_devices() -> Vec<AudioObjectID> {
    get_object_list(SYSTEM_OBJECT, kAudioHardwarePropertyDevices).unwrap_or_default()
}

pub fn transport_type(device: AudioObjectID) -> Option<u32> {
    // SAFETY: the property is a UInt32.
    unsafe { get_prop::<u32>(device, kAudioDevicePropertyTransportType).ok() }
}

/// The selected output data source (`'ispk'`, `'hdpn'`), where the device has
/// one.
pub fn output_data_source(device: AudioObjectID) -> Option<u32> {
    let addr = scoped_address(kAudioDevicePropertyDataSource, kAudioObjectPropertyScopeOutput);
    // SAFETY: the property is a UInt32.
    unsafe { get_prop_at::<u32>(device, addr).ok() }
}

/// Whether anything at all is running I/O on `device`, this process
/// included. One cheap read.
pub fn device_is_running_somewhere(device: AudioObjectID) -> Option<bool> {
    // SAFETY: the property is a UInt32.
    unsafe { get_prop::<u32>(device, kAudioDevicePropertyDeviceIsRunningSomewhere).ok() }.map(|v| v != 0)
}

/// This process as the HAL knows it, for `other_process_running_output`.
/// `None` when the HAL does not know it (yet).
fn own_process_object() -> Option<AudioObjectID> {
    let addr = address(kAudioHardwarePropertyTranslatePIDToProcessObject);
    let pid: libc::pid_t = std::process::id() as libc::pid_t;
    let mut object: AudioObjectID = UNKNOWN_OBJECT;
    let mut size = std::mem::size_of::<AudioObjectID>() as u32;
    // SAFETY: the qualifier is a pid_t, the result an AudioObjectID.
    let status = unsafe {
        AudioObjectGetPropertyData(
            SYSTEM_OBJECT,
            NonNull::from(&addr),
            std::mem::size_of::<libc::pid_t>() as u32,
            (&pid as *const libc::pid_t).cast::<c_void>(),
            NonNull::from(&mut size),
            NonNull::from(&mut object).cast::<c_void>(),
        )
    };
    (status == 0 && object != UNKNOWN_OBJECT).then_some(object)
}

/// Whether any process other than this one is playing audio right now.
/// `None` when the HAL cannot say (the process list is macOS 14.2+).
///
/// The output device's `DeviceIsRunningSomewhere` cannot answer this alone:
/// once our aggregate's IOProc runs, it counts as "somewhere". This walks the
/// HAL's process list instead, which costs two round trips to coreaudiod per
/// process (24 ms in all, measured): ask rarely, and only while the answer
/// matters. `system_tap::PlayingProbe` does the rationing.
pub fn other_process_running_output() -> Option<bool> {
    let processes = get_object_list(SYSTEM_OBJECT, kAudioHardwarePropertyProcessObjectList).ok()?;
    let own = own_process_object();
    let mut any_known = false;
    for process in processes {
        if Some(process) == own {
            continue;
        }
        // SAFETY: the property is a UInt32.
        match unsafe { get_prop::<u32>(process, kAudioProcessPropertyIsRunningOutput) } {
            Ok(0) => any_known = true,
            Ok(_) => return Some(true),
            Err(_) => {}
        }
    }
    // An empty or unreadable list says nothing.
    any_known.then_some(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs coreaudiod; prints what the HAL query costs"]
    fn asking_who_is_playing_is_cheap_enough_for_once_a_second() {
        let t = std::time::Instant::now();
        let mut answer = None;
        for _ in 0..10 {
            answer = other_process_running_output();
        }
        let per_call = t.elapsed() / 10;
        println!("hal: other_process_running_output = {answer:?}, {per_call:?} per call, own = {:?}", own_process_object());
        assert!(per_call < std::time::Duration::from_millis(30), "{per_call:?}");
    }

    #[test]
    fn statuses_render_as_four_char_codes() {
        assert_eq!(status_str(0x7768_6f3f), "2003332927 ('who?')");
        assert_eq!(status_str(0x2164_6576), "560227702 ('!dev')");
        assert_eq!(status_str(-50), "-50");
        assert_eq!(status_str(0), "0");
    }
}
