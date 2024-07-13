use std::{
    ffi::CStr,
    marker::PhantomData,
    ptr::{null, NonNull},
};

use asterisk_sys::bindings::{
    ast_category, ast_category_first, ast_category_get, ast_config, ast_config_load2, ast_flags,
    ast_module_info, ast_variable,
};

pub struct AsteriskConfig {
    raw: *mut ast_config,
}

pub struct ConfigCategory<'a> {
    category: NonNull<ast_category>,
    config: PhantomData<&'a AsteriskConfig>,
}

pub struct ConfigVariable<'a> {
    raw: NonNull<ast_variable>,
    config: PhantomData<&'a AsteriskConfig>,
}

pub enum LoadConfigError {
    MissingFile,
    FileUnchanged,
    FileInvalid,
    UnknownError,
}

impl AsteriskConfig {
    pub fn load(name: &CStr, module: &ast_module_info) -> Result<Self, LoadConfigError> {
        let cfg = unsafe { ast_config_load2(name.as_ptr(), module.name, ast_flags { flags: 0 }) };
        match cfg as isize {
            0 => Err(LoadConfigError::MissingFile),
            -1 => Err(LoadConfigError::FileUnchanged),
            -2 => Err(LoadConfigError::FileInvalid),
            ..=0 => Err(LoadConfigError::UnknownError),
            1.. => Ok(Self { raw: cfg }),
        }
    }

    pub fn category<'a>(&self, name: &'_ CStr) -> Option<ConfigCategory<'a>> {
        let category = unsafe { ast_category_get(self.raw, name.as_ptr(), null()) };
        Some(ConfigCategory {
            category: NonNull::new(category)?,
            config: PhantomData,
        })
    }
}

impl<'a> ConfigVariable<'a> {
    pub fn name(&self) -> &'a CStr {
        unsafe { CStr::from_ptr(self.raw.as_ref().name) }
    }

    pub fn value(&self) -> &'a CStr {
        unsafe { CStr::from_ptr(self.raw.as_ref().value) }
    }
}

pub struct VariableIterator<'a> {
    next_variable: *mut ast_variable,
    config: PhantomData<&'a AsteriskConfig>,
}

impl<'a> IntoIterator for &ConfigCategory<'a> {
    type Item = ConfigVariable<'a>;
    type IntoIter = VariableIterator<'a>;

    fn into_iter(self) -> Self::IntoIter {
        let root = unsafe { ast_category_first(self.category.as_ptr()) };

        VariableIterator {
            next_variable: root,
            config: PhantomData,
        }
    }
}

impl<'a> Iterator for VariableIterator<'a> {
    type Item = ConfigVariable<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        let next = NonNull::new(self.next_variable)?;
        self.next_variable = unsafe { next.as_ref() }.next;

        Some(ConfigVariable {
            raw: next,
            config: PhantomData,
        })
    }
}
