//! Identity of the current user and machine.

/// Windows username: `USERNAME` env var, then `GetUserNameW`, then `"unknown"`.
pub fn windows_username() -> String {
    if let Ok(u) = std::env::var("USERNAME") {
        let u = u.trim();
        if !u.is_empty() {
            return u.to_string();
        }
    }
    #[cfg(windows)]
    {
        if let Some(u) = get_user_name_w() {
            return u;
        }
    }
    "unknown".to_string()
}

/// `COMPUTERNAME`, falling back to `"unknown-pc"`.
pub fn machine_name() -> String {
    match std::env::var("COMPUTERNAME") {
        Ok(m) if !m.trim().is_empty() => m.trim().to_string(),
        _ => "unknown-pc".to_string(),
    }
}

#[cfg(windows)]
fn get_user_name_w() -> Option<String> {
    use windows_sys::Win32::System::WindowsProgramming::GetUserNameW;
    let mut buf = [0u16; 257];
    let mut len = buf.len() as u32;
    // SAFETY: buf is valid for `len` u16s; len is updated to include the NUL.
    let ok = unsafe { GetUserNameW(buf.as_mut_ptr(), &mut len) };
    if ok == 0 || len < 2 {
        return None;
    }
    let s = String::from_utf16_lossy(&buf[..(len as usize - 1)]);
    let s = s.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn username_non_empty() {
        assert!(!windows_username().is_empty());
    }

    #[test]
    fn machine_non_empty() {
        assert!(!machine_name().is_empty());
    }
}
