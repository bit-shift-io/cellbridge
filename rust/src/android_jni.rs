//! Narrow headless Android boundary; no Flutter engine required.
use jni::{
    JNIEnv,
    objects::{JObject, JString},
    sys::{jint, jstring},
};
use std::{
    os::fd::{FromRawFd, IntoRawFd, OwnedFd},
    panic::{AssertUnwindSafe, catch_unwind},
};

fn checked(env: &mut JNIEnv, operation: impl FnOnce(&mut JNIEnv) -> Result<(), String>) {
    let result = catch_unwind(AssertUnwindSafe(|| operation(env)));
    let error = match result {
        Ok(Ok(())) => return,
        Ok(Err(error)) => error,
        Err(_) => "native bridge failed".into(),
    };
    let _ = env.throw_new("java/lang/IllegalStateException", error);
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_cellbridge_NativeBridge_nativeTestConnection(
    mut env: JNIEnv,
    _this: JObject,
    config: JString,
    fd: jint,
) {
    checked(&mut env, |env| {
        if fd < 0 {
            return Err("invalid Wi-Fi socket descriptor".into());
        }
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        let config: String = env
            .get_string(&config)
            .map_err(|_| "cannot read bridge configuration")?
            .into();
        crate::native::test_connection_json(&config, owned.into_raw_fd())
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_cellbridge_NativeBridge_nativeStart(
    mut env: JNIEnv,
    _this: JObject,
    config: JString,
    fd: jint,
) {
    checked(&mut env, |env| {
        if fd < 0 {
            return Err("invalid Wi-Fi socket descriptor".into());
        }
        let owned = unsafe { OwnedFd::from_raw_fd(fd) };
        let config: String = env
            .get_string(&config)
            .map_err(|_| "cannot read bridge configuration")?
            .into();
        crate::native::start_json(&config, owned.into_raw_fd())
    });
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_cellbridge_NativeBridge_nativeStop(
    mut env: JNIEnv,
    _this: JObject,
) {
    checked(&mut env, |_| crate::native::stop());
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_dev_cellbridge_NativeBridge_nativeSnapshot(
    mut env: JNIEnv,
    _this: JObject,
) -> jstring {
    match catch_unwind(AssertUnwindSafe(|| {
        env.new_string(crate::native::snapshot_json())
    })) {
        Ok(Ok(string)) => string.into_raw(),
        _ => {
            let _ = env.throw_new(
                "java/lang/IllegalStateException",
                "cannot read bridge state",
            );
            std::ptr::null_mut()
        }
    }
}
