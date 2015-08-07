#![cfg(any(target_os = "windows", target_os = "linux", target_os = "android"))]
#![allow(unused_variables)]

use BuilderAttribs;
use ContextError;
use CreationError;
use GlContext;
use GlRequest;
use PixelFormat;
use Robustness;
use Api;

use libc;
use std::ffi::{CStr, CString};
use std::{mem, ptr};

pub mod ffi;

pub struct Context {
    egl: ffi::egl::Egl,
    display: ffi::egl::types::EGLDisplay,
    context: ffi::egl::types::EGLContext,
    surface: ffi::egl::types::EGLSurface,
    api: Api,
    pixel_format: PixelFormat,
}

impl Context {
    /// Start building an EGL context.
    ///
    /// This function initializes some things and chooses the pixel format.
    ///
    /// To finish the process, you must call `.finish(window)` on the `ContextPrototype`.
    pub fn new<'a>(egl: ffi::egl::Egl, builder: &'a BuilderAttribs<'a>,
                   native_display: Option<ffi::EGLNativeDisplayType>)
                   -> Result<ContextPrototype<'a>, CreationError>
    {
        if builder.sharing.is_some() {
            unimplemented!()
        }

        // the first step is to query the list of extensions without any display, if supported
        let extensions = unsafe {
            let p = egl.QueryString(ffi::egl::NO_DISPLAY, ffi::egl::EXTENSIONS as i32);

            // this possibility is available only with EGL 1.5 or EGL_EXT_platform_base, otherwise
            // `eglQueryString` returns an error
            if p.is_null() {
                None
            } else {
                let p = CStr::from_ptr(p);
                let list = String::from_utf8(p.to_bytes().to_vec()).unwrap_or_else(|_| format!(""));
                Some(list.split(' ').map(|e| e.to_string()).collect::<Vec<_>>())
            }
        };

        let display = unsafe {
            let display = egl.GetDisplay(native_display.unwrap_or(mem::transmute(ffi::egl::DEFAULT_DISPLAY)));
            if display.is_null() {
                return Err(CreationError::OsError("No EGL display connection available".to_string()));
            }
            display
        };

        let egl_version = unsafe {
            let mut major: ffi::egl::types::EGLint = mem::uninitialized();
            let mut minor: ffi::egl::types::EGLint = mem::uninitialized();

            if egl.Initialize(display, &mut major, &mut minor) == 0 {
                return Err(CreationError::OsError(format!("eglInitialize failed")))
            }

            (major, minor)
        };

        // getting the list of extensions for real
        let extensions = if let Some(extensions) = extensions {
            extensions

        } else if egl_version >= (1, 2) {
            let p = unsafe { CStr::from_ptr(egl.QueryString(display, ffi::egl::EXTENSIONS as i32)) };
            let list = String::from_utf8(p.to_bytes().to_vec()).unwrap_or_else(|_| format!(""));
            list.split(' ').map(|e| e.to_string()).collect::<Vec<_>>()

        } else {
            vec![]
        };

        // binding the right API and choosing the version
        let (version, api) = unsafe {
            match builder.gl_version {
                GlRequest::Latest => {
                    if egl_version >= (1, 4) {
                        if egl.BindAPI(ffi::egl::OPENGL_API) != 0 {
                            (None, Api::OpenGl)
                        } else if egl.BindAPI(ffi::egl::OPENGL_ES_API) != 0 {
                            (None, Api::OpenGlEs)
                        } else {
                            return Err(CreationError::OpenGlVersionNotSupported);
                        }
                    } else {
                        (None, Api::OpenGlEs)
                    }
                },
                GlRequest::Specific(Api::OpenGlEs, version) => {
                    if egl_version >= (1, 2) {
                        if egl.BindAPI(ffi::egl::OPENGL_ES_API) == 0 {
                            return Err(CreationError::OpenGlVersionNotSupported);
                        }
                    }
                    (Some(version), Api::OpenGlEs)
                },
                GlRequest::Specific(Api::OpenGl, version) => {
                    if egl_version < (1, 4) {
                        return Err(CreationError::OpenGlVersionNotSupported);
                    }
                    if egl.BindAPI(ffi::egl::OPENGL_API) == 0 {
                        return Err(CreationError::OpenGlVersionNotSupported);
                    }
                    (Some(version), Api::OpenGl)
                },
                GlRequest::Specific(_, _) => return Err(CreationError::OpenGlVersionNotSupported),
                GlRequest::GlThenGles { opengles_version, opengl_version } => {
                    if egl_version >= (1, 4) {
                        if egl.BindAPI(ffi::egl::OPENGL_API) != 0 {
                            (Some(opengl_version), Api::OpenGl)
                        } else if egl.BindAPI(ffi::egl::OPENGL_ES_API) != 0 {
                            (Some(opengles_version), Api::OpenGlEs)
                        } else {
                            return Err(CreationError::OpenGlVersionNotSupported);
                        }
                    } else {
                        (Some(opengles_version), Api::OpenGlEs)
                    }
                },
            }
        };

        let configs = unsafe { try!(enumerate_configs(&egl, display, &egl_version, api, version)) };
        let (config_id, pixel_format) = try!(builder.choose_pixel_format(configs.into_iter()));

        Ok(ContextPrototype {
            builder: builder,
            egl: egl,
            display: display,
            egl_version: egl_version,
            extensions: extensions,
            api: api,
            version: version,
            config_id: config_id,
            pixel_format: pixel_format,
        })
    }
}

impl GlContext for Context {
    unsafe fn make_current(&self) -> Result<(), ContextError> {
        let ret = self.egl.MakeCurrent(self.display, self.surface, self.surface, self.context);

        if ret == 0 {
            match self.egl.GetError() as u32 {
                ffi::egl::CONTEXT_LOST => return Err(ContextError::ContextLost),
                err => panic!("eglMakeCurrent failed (eglGetError returned 0x{:x})", err)
            }

        } else {
            Ok(())
        }
    }

    fn is_current(&self) -> bool {
        unsafe { self.egl.GetCurrentContext() == self.context }
    }

    fn get_proc_address(&self, addr: &str) -> *const libc::c_void {
        let addr = CString::new(addr.as_bytes()).unwrap();
        let addr = addr.as_ptr();
        unsafe {
            self.egl.GetProcAddress(addr) as *const _
        }
    }

    fn swap_buffers(&self) -> Result<(), ContextError> {
        let ret = unsafe {
            self.egl.SwapBuffers(self.display, self.surface)
        };

        if ret == 0 {
            match unsafe { self.egl.GetError() } as u32 {
                ffi::egl::CONTEXT_LOST => return Err(ContextError::ContextLost),
                err => panic!("eglSwapBuffers failed (eglGetError returned 0x{:x})", err)
            }

        } else {
            Ok(())
        }
    }

    fn get_api(&self) -> Api {
        self.api
    }

    fn get_pixel_format(&self) -> PixelFormat {
        self.pixel_format.clone()
    }
}

unsafe impl Send for Context {}
unsafe impl Sync for Context {}

impl Drop for Context {
    fn drop(&mut self) {
        unsafe {
            // we don't call MakeCurrent(0, 0) because we are not sure that the context
            // is still the current one
            self.egl.DestroyContext(self.display, self.context);
            self.egl.DestroySurface(self.display, self.surface);
            self.egl.Terminate(self.display);
        }
    }
}

pub struct ContextPrototype<'a> {
    builder: &'a BuilderAttribs<'a>,
    egl: ffi::egl::Egl,
    display: ffi::egl::types::EGLDisplay,
    egl_version: (ffi::egl::types::EGLint, ffi::egl::types::EGLint),
    extensions: Vec<String>,
    api: Api,
    version: Option<(u8, u8)>,
    config_id: ffi::egl::types::EGLConfig,
    pixel_format: PixelFormat,
}

impl<'a> ContextPrototype<'a> {
    pub fn get_native_visual_id(&self) -> ffi::egl::types::EGLint {
        let mut value = unsafe { mem::uninitialized() };
        let ret = unsafe { self.egl.GetConfigAttrib(self.display, self.config_id,
                                                    ffi::egl::NATIVE_VISUAL_ID
                                                    as ffi::egl::types::EGLint, &mut value) };
        if ret == 0 { panic!("eglGetConfigAttrib failed") };
        value
    }

    pub fn finish(self, native_window: ffi::EGLNativeWindowType)
                  -> Result<Context, CreationError>
    {
        let surface = unsafe {
            let surface = self.egl.CreateWindowSurface(self.display, self.config_id, native_window,
                                                       ptr::null());
            if surface.is_null() {
                return Err(CreationError::OsError(format!("eglCreateWindowSurface failed")))
            }
            surface
        };

        self.finish_impl(surface)
    }

    pub fn finish_pbuffer(self) -> Result<Context, CreationError> {
        let dimensions = self.builder.dimensions.unwrap_or((800, 600));

        let attrs = &[
            ffi::egl::WIDTH as libc::c_int, dimensions.0 as libc::c_int,
            ffi::egl::HEIGHT as libc::c_int, dimensions.1 as libc::c_int,
            ffi::egl::NONE as libc::c_int,
        ];

        let surface = unsafe {
            let surface = self.egl.CreatePbufferSurface(self.display, self.config_id,
                                                        attrs.as_ptr());
            if surface.is_null() {
                return Err(CreationError::OsError(format!("eglCreatePbufferSurface failed")))
            }
            surface
        };

        self.finish_impl(surface)
    }

    fn finish_impl(self, surface: ffi::egl::types::EGLSurface)
                   -> Result<Context, CreationError>
    {
        let context = unsafe {
            if let Some(version) = self.version {
                try!(create_context(&self.egl, self.display, &self.egl_version,
                                    &self.extensions, self.api, version, self.config_id,
                                    self.builder.gl_debug, self.builder.gl_robustness))

            } else if self.api == Api::OpenGlEs {
                if let Ok(ctxt) = create_context(&self.egl, self.display, &self.egl_version,
                                                 &self.extensions, self.api, (2, 0), self.config_id,
                                                 self.builder.gl_debug, self.builder.gl_robustness)
                {
                    ctxt
                } else if let Ok(ctxt) = create_context(&self.egl, self.display, &self.egl_version,
                                                        &self.extensions, self.api, (1, 0),
                                                        self.config_id, self.builder.gl_debug,
                                                        self.builder.gl_robustness)
                {
                    ctxt
                } else {
                    return Err(CreationError::OpenGlVersionNotSupported);
                }

            } else {
                if let Ok(ctxt) = create_context(&self.egl, self.display, &self.egl_version,
                                                 &self.extensions, self.api, (3, 2), self.config_id,
                                                 self.builder.gl_debug, self.builder.gl_robustness)
                {
                    ctxt
                } else if let Ok(ctxt) = create_context(&self.egl, self.display, &self.egl_version,
                                                        &self.extensions, self.api, (3, 1),
                                                        self.config_id, self.builder.gl_debug,
                                                        self.builder.gl_robustness)
                {
                    ctxt
                } else if let Ok(ctxt) = create_context(&self.egl, self.display, &self.egl_version,
                                                        &self.extensions, self.api, (1, 0),
                                                        self.config_id, self.builder.gl_debug,
                                                        self.builder.gl_robustness)
                {
                    ctxt
                } else {
                    return Err(CreationError::OpenGlVersionNotSupported);
                }
            }
        };

        Ok(Context {
            egl: self.egl,
            display: self.display,
            context: context,
            surface: surface,
            api: self.api,
            pixel_format: self.pixel_format,
        })
    }
}

unsafe fn enumerate_configs(egl: &ffi::egl::Egl, display: ffi::egl::types::EGLDisplay,
                            egl_version: &(ffi::egl::types::EGLint, ffi::egl::types::EGLint),
                            api: Api, version: Option<(u8, u8)>)
                            -> Result<Vec<(ffi::egl::types::EGLConfig, PixelFormat)>, CreationError>
{
    let mut num_configs = mem::uninitialized();
    if egl.GetConfigs(display, ptr::null_mut(), 0, &mut num_configs) == 0 {
        return Err(CreationError::OsError(format!("eglGetConfigs failed")));
    }

    let mut configs_ids = Vec::with_capacity(num_configs as usize);
    if egl.GetConfigs(display, configs_ids.as_mut_ptr(),
                      configs_ids.capacity() as ffi::egl::types::EGLint,
                      &mut num_configs) == 0
    {
        return Err(CreationError::OsError(format!("eglGetConfigs failed")));
    }
    configs_ids.set_len(num_configs as usize);

    // analyzing each config
    let mut result = Vec::with_capacity(num_configs as usize);
    for config_id in configs_ids {
        macro_rules! attrib {
            ($egl:expr, $display:expr, $config:expr, $attr:expr) => (
                {
                    let mut value = mem::uninitialized();
                    let res = $egl.GetConfigAttrib($display, $config,
                                                   $attr as ffi::egl::types::EGLint, &mut value);
                    if res == 0 {
                        return Err(CreationError::OsError(format!("eglGetConfigAttrib failed")));
                    }
                    value
                }
            )
        };

        let renderable = attrib!(egl, display, config_id, ffi::egl::RENDERABLE_TYPE) as u32;
        let conformant = attrib!(egl, display, config_id, ffi::egl::CONFORMANT) as u32;

        if api == Api::OpenGlEs {
            if let Some(version) = version {
                if version.0 == 3 && (renderable & ffi::egl::OPENGL_ES3_BIT == 0 ||
                                      conformant & ffi::egl::OPENGL_ES3_BIT == 0)
                {
                    continue;
                }

                if version.0 == 2 && (renderable & ffi::egl::OPENGL_ES2_BIT == 0 ||
                                      conformant & ffi::egl::OPENGL_ES2_BIT == 0)
                {
                    continue;
                }

                if version.0 == 1 && (renderable & ffi::egl::OPENGL_ES_BIT == 0 ||
                                      conformant & ffi::egl::OPENGL_ES_BIT == 0)
                {
                    continue;
                }
            }

        } else if api == Api::OpenGl {
            if renderable & ffi::egl::OPENGL_BIT == 0 ||
               conformant & ffi::egl::OPENGL_BIT == 0
            {
                continue;
            }
        }

        if attrib!(egl, display, config_id, ffi::egl::SURFACE_TYPE) &
                                        (ffi::egl::WINDOW_BIT | ffi::egl::PBUFFER_BIT) as i32 == 0
        {
            continue;
        }

        if attrib!(egl, display, config_id, ffi::egl::TRANSPARENT_TYPE) != ffi::egl::NONE as i32 {
            continue;
        }

        if attrib!(egl, display, config_id, ffi::egl::COLOR_BUFFER_TYPE) != ffi::egl::RGB_BUFFER as i32 {
            continue;
        }

        result.push((config_id, PixelFormat {
            hardware_accelerated: attrib!(egl, display, config_id, ffi::egl::CONFIG_CAVEAT)
                                          != ffi::egl::SLOW_CONFIG as i32,
            color_bits: attrib!(egl, display, config_id, ffi::egl::RED_SIZE) as u8 +
                        attrib!(egl, display, config_id, ffi::egl::BLUE_SIZE) as u8 +
                        attrib!(egl, display, config_id, ffi::egl::GREEN_SIZE) as u8,
            alpha_bits: attrib!(egl, display, config_id, ffi::egl::ALPHA_SIZE) as u8,
            depth_bits: attrib!(egl, display, config_id, ffi::egl::DEPTH_SIZE) as u8,
            stencil_bits: attrib!(egl, display, config_id, ffi::egl::STENCIL_SIZE) as u8,
            stereoscopy: false,
            double_buffer: true,
            multisampling: match attrib!(egl, display, config_id, ffi::egl::SAMPLES) {
                0 | 1 => None,
                a => Some(a as u16),
            },
            srgb: false,        // TODO: use EGL_KHR_gl_colorspace to know that
        }));
    }

    Ok(result)
}

unsafe fn create_context(egl: &ffi::egl::Egl, display: ffi::egl::types::EGLDisplay,
                         egl_version: &(ffi::egl::types::EGLint, ffi::egl::types::EGLint),
                         extensions: &[String], api: Api, version: (u8, u8),
                         config_id: ffi::egl::types::EGLConfig, gl_debug: bool,
                         gl_robustness: Robustness)
                         -> Result<ffi::egl::types::EGLContext, CreationError>
{
    let mut context_attributes = Vec::with_capacity(10);
    let mut flags = 0;

    if egl_version >= &(1, 5) || extensions.iter().find(|s| s == &"EGL_KHR_create_context")
                                                  .is_some()
    {
        context_attributes.push(ffi::egl::CONTEXT_MAJOR_VERSION as i32);
        context_attributes.push(version.0 as i32);
        context_attributes.push(ffi::egl::CONTEXT_MINOR_VERSION as i32);
        context_attributes.push(version.1 as i32);

        // handling robustness
        let supports_robustness = egl_version >= &(1, 5) ||
                                  extensions.iter()
                                            .find(|s| s == &"EGL_EXT_create_context_robustness")
                                            .is_some();

        match gl_robustness {
            Robustness::NotRobust => (),

            Robustness::NoError => {
                if extensions.iter().find(|s| s == &"EGL_KHR_create_context_no_error").is_some() {
                    context_attributes.push(ffi::egl::CONTEXT_OPENGL_NO_ERROR_KHR as libc::c_int);
                    context_attributes.push(1);
                }
            },

            Robustness::RobustNoResetNotification => {
                if supports_robustness {
                    context_attributes.push(ffi::egl::CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY
                                            as libc::c_int);
                    context_attributes.push(ffi::egl::NO_RESET_NOTIFICATION as libc::c_int);
                    flags = flags | ffi::egl::CONTEXT_OPENGL_ROBUST_ACCESS as libc::c_int;
                } else {
                    return Err(CreationError::RobustnessNotSupported);
                }
            },

            Robustness::TryRobustNoResetNotification => {
                if supports_robustness {
                    context_attributes.push(ffi::egl::CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY
                                            as libc::c_int);
                    context_attributes.push(ffi::egl::NO_RESET_NOTIFICATION as libc::c_int);
                    flags = flags | ffi::egl::CONTEXT_OPENGL_ROBUST_ACCESS as libc::c_int;
                }
            },

            Robustness::RobustLoseContextOnReset => {
                if supports_robustness {
                    context_attributes.push(ffi::egl::CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY
                                            as libc::c_int);
                    context_attributes.push(ffi::egl::LOSE_CONTEXT_ON_RESET as libc::c_int);
                    flags = flags | ffi::egl::CONTEXT_OPENGL_ROBUST_ACCESS as libc::c_int;
                } else {
                    return Err(CreationError::RobustnessNotSupported);
                }
            },

            Robustness::TryRobustLoseContextOnReset => {
                if supports_robustness {
                    context_attributes.push(ffi::egl::CONTEXT_OPENGL_RESET_NOTIFICATION_STRATEGY
                                            as libc::c_int);
                    context_attributes.push(ffi::egl::LOSE_CONTEXT_ON_RESET as libc::c_int);
                    flags = flags | ffi::egl::CONTEXT_OPENGL_ROBUST_ACCESS as libc::c_int;
                }
            },
        }

        if gl_debug {
            if egl_version >= &(1, 5) {
                context_attributes.push(ffi::egl::CONTEXT_OPENGL_DEBUG as i32);
                context_attributes.push(ffi::egl::TRUE as i32);
            }

            // TODO: using this flag sometimes generates an error
            //       there was a change in the specs that added this flag, so it may not be
            //       supported everywhere ; however it is not possible to know whether it is
            //       supported or not
            //flags = flags | ffi::egl::CONTEXT_OPENGL_DEBUG_BIT_KHR as i32;
        }

        context_attributes.push(ffi::egl::CONTEXT_FLAGS_KHR as i32);
        context_attributes.push(flags);

    } else if egl_version >= &(1, 3) && api == Api::OpenGlEs {
        // robustness is not supported
        match gl_robustness {
            Robustness::RobustNoResetNotification | Robustness::RobustLoseContextOnReset => {
                return Err(CreationError::RobustnessNotSupported);
            },
            _ => ()
        }

        context_attributes.push(ffi::egl::CONTEXT_CLIENT_VERSION as i32);
        context_attributes.push(version.0 as i32);
    }

    context_attributes.push(ffi::egl::NONE as i32);

    let context = egl.CreateContext(display, config_id, ptr::null(),
                                    context_attributes.as_ptr());

    if context.is_null() {
        match egl.GetError() as u32 {
            ffi::egl::BAD_ATTRIBUTE => return Err(CreationError::OpenGlVersionNotSupported),
            e => panic!("eglCreateContext failed: 0x{:x}", e),
        }
    }

    Ok(context)
}
