use crate::nativescript::init::ClassBuilder;
use crate::nativescript::Map;
use crate::nativescript::MapMut;
use crate::nativescript::UserData;
use crate::object;
use crate::private::get_api;
use crate::sys;
use crate::FromVariant;
use crate::FromVariantError;
use crate::GodotObject;
use crate::GodotString;
use crate::Instanciable;
use crate::RefCounted;
use crate::ToVariant;
use crate::Variant;

/// Trait used for describing and initializing a Godot script class.
///
/// This trait is used to provide data and functionality to the
/// "data-part" of the class, such as name, initialization and information
/// about exported properties.
///
/// For exported methods, see the [`NativeClassMethods`] trait.
///
/// [`NativeClassMethods`]: ./trait.NativeClassMethods.html
pub trait NativeClass: Sized + 'static {
    /// Base type of the class.
    ///
    /// In Godot, scripting languages can define "script instances" which can be
    /// attached to objects. Because of the dynamic nature, the intended "inheritance"
    /// is not easily implementable properly.
    ///
    /// Instead, delegation is used and most calls to a Godot object query the script instance
    /// first. This way, some methods can be "overwritten" and new ones can be exposed.
    ///
    /// This only works when using so called "variant calls", since the querying of the script
    /// instance is performed there.
    /// When not using variant calls, any direct(*) calls have to be made to the Godot object
    /// directly.
    ///
    /// The base type describes the "most general" type of object this script class can be
    /// attached to.
    ///
    /// *(\*)*: GDNative enables use of "ptrcall"s, which are wrappers for function pointers.
    /// Those do not do explicit checks for script implementations **unless the method
    /// implementation does**.
    type Base: GodotObject;

    /// User-data wrapper type of the class.
    ///
    /// See module-level documentation on `user_data` for more info.
    type UserData: UserData<Target = Self>;

    /// The name of the class.
    ///
    /// In GDNative+NativeScript many classes can be defined in one dynamic library.
    /// To identify which class has to be used, a library-unique name has to be given.
    fn class_name() -> &'static str;

    /// Function that creates a value of `Self`, used for the script-instance.
    ///
    /// This function has a reference to the owner object as a parameter, which can be used to
    /// set state on the owner upon creation or to query values
    fn init(owner: Self::Base) -> Self;

    /// Register any exported properties to Godot.
    #[inline]
    fn register_properties(_builder: &ClassBuilder<Self>) {}
}

/// Trait used to provide information of Godot-exposed methods of a script class.
pub trait NativeClassMethods: NativeClass {
    /// Function that registers all exposed methods to Godot.
    fn register(builder: &ClassBuilder<Self>);
}

/// A reference to a GodotObject with a rust NativeClass attached.
#[derive(Eq, PartialEq, Ord, PartialOrd, Debug)]
pub struct Instance<T: NativeClass> {
    owner: T::Base,
    script: T::UserData,
}

impl<T: NativeClass> Instance<T> {
    /// Creates a `T::Base` with the script `T` attached. Both `T::Base` and `T` must have zero
    /// argument constructors.
    ///
    /// Must be called after the library is initialized.
    #[inline]
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self
    where
        T::Base: Instanciable,
    {
        unsafe {
            let gd_api = get_api();

            // The API functions take NUL-terminated C strings. &CStr is not used for its runtime cost.
            let class_name = b"NativeScript\0".as_ptr() as *const libc::c_char;
            let ctor = (gd_api.godot_get_class_constructor)(class_name).unwrap();
            let set_class_name = (gd_api.godot_method_bind_get_method)(
                class_name,
                b"set_class_name\0".as_ptr() as *const libc::c_char,
            );
            let set_library = (gd_api.godot_method_bind_get_method)(
                class_name,
                b"set_library\0".as_ptr() as *const libc::c_char,
            );
            let object_set_script =
                crate::generated::object::ObjectMethodTable::get(gd_api).set_script;

            let native_script = ctor();
            object::init_ref_count(native_script);

            let script_class_name = GodotString::from(T::class_name());
            let mut args: [*const libc::c_void; 1] = [script_class_name.sys() as *const _];
            (gd_api.godot_method_bind_ptrcall)(
                set_class_name,
                native_script,
                args.as_mut_ptr(),
                std::ptr::null_mut(),
            );

            let mut args: [*const libc::c_void; 1] = [crate::private::get_gdnative_library_sys()];
            (gd_api.godot_method_bind_ptrcall)(
                set_library,
                native_script,
                args.as_mut_ptr(),
                std::ptr::null_mut(),
            );

            let owner = T::Base::construct();

            assert_ne!(
                std::ptr::null_mut(),
                owner.to_sys(),
                "base object should not be null"
            );

            let mut args: [*const libc::c_void; 1] = [native_script as *const _];
            (gd_api.godot_method_bind_ptrcall)(
                object_set_script,
                owner.to_sys(),
                args.as_mut_ptr(),
                std::ptr::null_mut(),
            );

            let script_ptr =
                (gd_api.godot_nativescript_get_userdata)(owner.to_sys()) as *const libc::c_void;

            assert_ne!(
                std::ptr::null(),
                script_ptr,
                "script instance should not be null"
            );

            let script = T::UserData::clone_from_user_data_unchecked(script_ptr);

            object::unref(native_script);

            Instance { owner, script }
        }
    }

    #[inline]
    pub fn into_base(self) -> T::Base {
        self.owner
    }

    #[inline]
    pub fn into_script(self) -> T::UserData {
        self.script
    }

    #[inline]
    pub fn decouple(self) -> (T::Base, T::UserData) {
        (self.owner, self.script)
    }

    #[inline]
    pub fn base(&self) -> &T::Base {
        &self.owner
    }

    #[inline]
    pub fn script(&self) -> &T::UserData {
        &self.script
    }

    /// Try to downcast `T::Base` to `Instance<T>`. This safe version can only be used with
    /// reference counted base classes.
    #[inline]
    pub fn try_from_base(owner: T::Base) -> Option<Self>
    where
        T::Base: RefCounted,
    {
        unsafe { Self::try_from_unsafe_base(owner) }
    }

    /// Try to downcast `T::Base` to `Instance<T>`.
    ///
    /// # Safety
    ///
    /// It's up to the caller to ensure that `owner` points to a valid Godot object, and
    /// that it will not be freed until this function returns. Otherwise, it is undefined
    /// behavior to call this function and/or use its return value.
    #[inline]
    pub unsafe fn try_from_unsafe_base(owner: T::Base) -> Option<Self> {
        let type_tag = (get_api().godot_nativescript_get_type_tag)(owner.to_sys());
        if type_tag.is_null() {
            return None;
        }

        if !crate::nativescript::type_tag::check::<T>(type_tag) {
            return None;
        }

        Some(Self::from_sys_unchecked(owner.to_sys()))
    }

    /// Calls a function with a NativeClass instance and its owner, and returns its return
    /// value. Can be used on reference counted types for multiple times.
    #[inline]
    pub fn map<F, U>(&self, op: F) -> Result<U, <T::UserData as Map>::Err>
    where
        T::UserData: Map,
        F: FnOnce(&T, T::Base) -> U,
    {
        self.script.map(|script| op(script, self.owner.claim()))
    }

    /// Calls a function with a NativeClass instance and its owner, and returns its return
    /// value. Can be used on reference counted types for multiple times.
    #[inline]
    pub fn map_mut<F, U>(&self, op: F) -> Result<U, <T::UserData as MapMut>::Err>
    where
        T::UserData: MapMut,
        F: FnOnce(&mut T, T::Base) -> U,
    {
        self.script.map_mut(|script| op(script, self.owner.claim()))
    }

    #[doc(hidden)]
    #[inline]
    pub unsafe fn from_sys_unchecked(ptr: *mut sys::godot_object) -> Self {
        let api = get_api();
        let user_data = (api.godot_nativescript_get_userdata)(ptr);
        Self::from_raw(ptr, user_data)
    }

    #[doc(hidden)]
    #[inline]
    pub unsafe fn from_raw(ptr: *mut sys::godot_object, user_data: *mut libc::c_void) -> Self {
        let owner = T::Base::from_sys(ptr);
        let script_ptr = user_data as *const libc::c_void;
        let script = T::UserData::clone_from_user_data_unchecked(script_ptr);
        Instance { owner, script }
    }
}

impl<T> Clone for Instance<T>
where
    T: NativeClass,
    T::Base: RefCounted,
{
    #[inline]
    fn clone(&self) -> Self {
        Instance {
            owner: self.owner.new_ref(),
            script: self.script.clone(),
        }
    }
}

impl<T> ToVariant for Instance<T>
where
    T: NativeClass,
    T::Base: ToVariant,
{
    #[inline]
    fn to_variant(&self) -> Variant {
        self.owner.to_variant()
    }
}

impl<T> FromVariant for Instance<T>
where
    T: NativeClass,
    T::Base: FromVariant + RefCounted,
{
    #[inline]
    fn from_variant(variant: &Variant) -> Result<Self, FromVariantError> {
        let owner = T::Base::from_variant(variant)?;
        Self::try_from_base(owner).ok_or(FromVariantError::InvalidInstance {
            expected: T::class_name(),
        })
    }
}