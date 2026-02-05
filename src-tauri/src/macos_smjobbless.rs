use std::ffi::{CStr, CString};
use std::path::Path;

extern "C" {
	fn smjobbless_install(label: *const i8, error_out: *mut *mut i8) -> i32;

	fn smhelper_start_singbox(
		label: *const i8,
		singbox_path: *const i8,
		config_path: *const i8,
		args_json: *const i8,
		error_out: *mut *mut i8,
	) -> i32;

	fn smhelper_stop_singbox(label: *const i8, error_out: *mut *mut i8) -> i32;

	fn smhelper_free(p: *mut core::ffi::c_void);
}

fn take_err(ptr: *mut i8) -> Option<String> {
	if ptr.is_null() {
		return None;
	}
	unsafe {
		let s = CStr::from_ptr(ptr).to_string_lossy().to_string();
		smhelper_free(ptr as *mut _);
		Some(s)
	}
}

pub fn install_helper(label: &str) -> Result<(), String> {
	let c_label = CString::new(label).map_err(|e| e.to_string())?;
	let mut err: *mut i8 = core::ptr::null_mut();

	let ok = unsafe { smjobbless_install(c_label.as_ptr(), &mut err) } != 0;
	if ok {
		Ok(())
	} else {
		Err(take_err(err).unwrap_or_else(|| "SMJobBless failed (no error)".to_string()))
	}
}

pub fn helper_start_singbox(label: &str, singbox_path: &Path, config_path: &Path, args: &[String]) -> Result<(), String> {
	let c_label = CString::new(label).map_err(|e| e.to_string())?;
	let c_sing = CString::new(singbox_path.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;
	let c_cfg = CString::new(config_path.to_string_lossy().as_bytes()).map_err(|e| e.to_string())?;

	let args_json = serde_json::to_string(args).map_err(|e| e.to_string())?;
	let c_args = CString::new(args_json).map_err(|e| e.to_string())?;

	let mut err: *mut i8 = core::ptr::null_mut();
	let ok = unsafe {
		smhelper_start_singbox(
			c_label.as_ptr(),
			c_sing.as_ptr(),
			c_cfg.as_ptr(),
			c_args.as_ptr(),
			&mut err,
		)
	} != 0;

	if ok {
		Ok(())
	} else {
		Err(take_err(err).unwrap_or_else(|| "helper startSingBox failed (no error)".to_string()))
	}
}

pub fn helper_stop_singbox(label: &str) -> Result<(), String> {
	let c_label = CString::new(label).map_err(|e| e.to_string())?;
	let mut err: *mut i8 = core::ptr::null_mut();
	let ok = unsafe { smhelper_stop_singbox(c_label.as_ptr(), &mut err) } != 0;

	if ok {
		Ok(())
	} else {
		Err(take_err(err).unwrap_or_else(|| "helper stopSingBox failed (no error)".to_string()))
	}
}
