use std::ffi::{CStr, CString};
use std::os::raw::{c_char};

extern "C" {
	fn ultunnel_smjobbless_install(label: *const c_char) -> bool;
	fn ultunnel_smjobbless_last_error() -> *const c_char;
}

pub fn smjobbless_install(label: &str) -> Result<(), String> {
	let c = CString::new(label).map_err(|e| e.to_string())?;
	let ok = unsafe { ultunnel_smjobbless_install(c.as_ptr()) };
	if ok {
		Ok(())
	} else {
		let ptr = unsafe { ultunnel_smjobbless_last_error() };
		let msg = if ptr.is_null() {
			"unknown error".to_string()
		} else {
			unsafe { CStr::from_ptr(ptr) }.to_string_lossy().to_string()
		};
		Err(msg)
	}
}
