//! The fonts Settings offers. Rune draws only the fonts it's handed at launch and a few system
//! files it loads itself (Inter, SF, SF Mono, Menlo); any other family falls back to Inter. So the
//! lists are short, and Hyper hands Rune the files of the ones it lacks.

use std::ffi::{CStr, CString, c_char, c_void};
use std::path::PathBuf;
use std::sync::OnceLock;

use objc2::rc::autoreleasepool;
use objc2::runtime::AnyObject;
use objc2::{class, msg_send};

/// A family Settings offers: the name Rune is given, and the name shown.
pub type Font = (&'static str, &'static str);

/// Interface fonts. Rune has both: Inter built in, and SF as "System Font".
pub const INTERFACE: [Font; 2] = [("Inter", "Inter"), ("System Font", "System")];

/// Session fonts, offered when installed: macOS's, then favourites from elsewhere.
const SESSION: [Font; 6] = [
    ("ui-monospace", "System Monospace"),
    ("Menlo", "Menlo"),
    ("Monaco", "Monaco"),
    ("Fira Code", "Fira Code"),
    ("Geist Mono", "Geist Mono"),
    ("JetBrains Mono", "JetBrains Mono"),
];

/// Families Rune loads from the system itself.
const RUNE_LOADS: [&str; 2] = ["ui-monospace", "Menlo"];

/// The installed session fonts, with the files of those Rune doesn't load itself. Core Text
/// answers in about a millisecond per family.
fn installed() -> &'static [(Font, Vec<PathBuf>)] {
    static INSTALLED: OnceLock<Vec<(Font, Vec<PathBuf>)>> = OnceLock::new();
    INSTALLED.get_or_init(|| {
        SESSION.into_iter().filter_map(|font| {
            if RUNE_LOADS.contains(&font.0) {
                return Some((font, Vec::new()));
            }
            let files = family_files(font.0);
            (!files.is_empty()).then_some((font, files))
        }).collect()
    })
}

/// The session fonts to offer.
pub fn session() -> Vec<Font> {
    installed().iter().map(|(font, _)| *font).collect()
}

/// Whether Settings offers `family`, for the session (`mono`) or the interface.
pub fn offered(family: &str, mono: bool) -> bool {
    if mono { installed().iter().any(|((f, _), _)| *f == family) } else { INTERFACE.iter().any(|(f, _)| *f == family) }
}

/// The name Settings shows for `family`.
pub fn name(family: &str) -> &str {
    INTERFACE.iter().chain(SESSION.iter()).find(|(f, _)| *f == family).map_or(family, |(_, name)| name)
}

/// The faces Rune has without being handed them: Inter, which handing it any fonts replaces.
const INTER: [&[u8]; 3] = [
    include_bytes!("../assets/fonts/Inter-Regular.ttf"),
    include_bytes!("../assets/fonts/Inter-Medium.ttf"),
    include_bytes!("../assets/fonts/Inter-SemiBold.ttf"),
];

/// The fonts to give `AppSpec::fonts`: Inter and the session fonts Rune doesn't load itself.
/// The files are mapped, not read, so only the glyphs drawn are loaded.
pub fn for_rune() -> &'static [&'static [u8]] {
    static FONTS: OnceLock<&'static [&'static [u8]]> = OnceLock::new();
    FONTS.get_or_init(|| {
        let mut fonts: Vec<&'static [u8]> = INTER.to_vec();
        fonts.extend(installed().iter().flat_map(|(_, files)| files).filter_map(|f| map(f)));
        fonts.leak()
    })
}

type CFTypeRef = *const c_void;

#[link(name = "CoreText", kind = "framework")]
unsafe extern "C" {
    fn CTFontDescriptorCreateWithAttributes(attributes: CFTypeRef) -> CFTypeRef;
    fn CTFontDescriptorCreateMatchingFontDescriptors(descriptor: CFTypeRef, mandatory: CFTypeRef) -> CFTypeRef;
    fn CTFontDescriptorCopyAttribute(descriptor: CFTypeRef, attribute: CFTypeRef) -> CFTypeRef;
    static kCTFontFamilyNameAttribute: CFTypeRef;
    static kCTFontURLAttribute: CFTypeRef;
}

#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFRelease(object: CFTypeRef);
}

/// The files of `family`'s faces; none when it isn't installed. Its objects are toll-free
/// bridged, so Foundation reads them.
fn family_files(family: &str) -> Vec<PathBuf> {
    let Ok(name) = CString::new(family) else { return Vec::new() };
    autoreleasepool(|_| unsafe {
        let name: *mut AnyObject = msg_send![class!(NSString), stringWithUTF8String: name.as_ptr()];
        let attributes: *mut AnyObject = msg_send![class!(NSDictionary), dictionaryWithObject: name, forKey: kCTFontFamilyNameAttribute as *mut AnyObject];
        let descriptor = CTFontDescriptorCreateWithAttributes(attributes as CFTypeRef);
        let matches = CTFontDescriptorCreateMatchingFontDescriptors(descriptor, std::ptr::null());
        let mut files = Vec::new();
        for face in each(matches) {
            let url = CTFontDescriptorCopyAttribute(face as CFTypeRef, kCTFontURLAttribute);
            if !url.is_null() {
                let path: *mut AnyObject = msg_send![url as *mut AnyObject, path];
                files.extend(string(path).map(PathBuf::from).filter(|p| !files.contains(p)));
            }
            release(url);
        }
        release(matches);
        release(descriptor);
        files
    })
}

/// `path` mapped read-only for the life of the process.
fn map(path: &std::path::Path) -> Option<&'static [u8]> {
    use std::os::fd::AsRawFd;
    let file = std::fs::File::open(path).ok()?;
    let len = usize::try_from(file.metadata().ok()?.len()).ok().filter(|&n| n > 0)?;
    let ptr = unsafe { libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ, libc::MAP_PRIVATE, file.as_raw_fd(), 0) };
    (ptr != libc::MAP_FAILED).then(|| unsafe { std::slice::from_raw_parts(ptr.cast::<u8>(), len) })
}

unsafe fn release(object: CFTypeRef) {
    if !object.is_null() {
        unsafe { CFRelease(object) };
    }
}

unsafe fn each(array: CFTypeRef) -> Vec<*mut AnyObject> {
    let array = array as *mut AnyObject;
    if array.is_null() {
        return Vec::new();
    }
    let count: usize = unsafe { msg_send![array, count] };
    (0..count).map(|i| unsafe { msg_send![array, objectAtIndex: i] }).collect()
}

unsafe fn string(s: *mut AnyObject) -> Option<String> {
    if s.is_null() {
        return None;
    }
    let utf8: *const c_char = unsafe { msg_send![s, UTF8String] };
    (!utf8.is_null()).then(|| unsafe { CStr::from_ptr(utf8) }.to_string_lossy().into_owned())
}

#[cfg(test)]
mod tests {
    use rune::TextStyle;
    use rune::text::TextSystem;

    /// The width of `s` in `style`, drawn with the fonts Hyper hands Rune.
    fn width(families: (&str, &str), s: &str, style: TextStyle) -> f32 {
        let _scope = rune::text::with_families(families.0, families.1);
        TextSystem::new(super::for_rune(), families).layout_rich(s, &[], style, 1.0, None).full_width()
    }

    /// Rune falls back to Inter for a family it wasn't handed, which spreads a terminal's
    /// cells (measured on "M") apart from its narrower letters.
    #[test]
    fn every_session_font_offered_draws_monospaced() {
        let offered = super::session();
        assert!(offered.iter().any(|(f, _)| *f == "Monaco"), "{offered:?}");
        let style = TextStyle::new(16.0, 400.0).mono();
        for (family, _) in offered {
            let (narrow, wide) = (width(("Inter", family), "iiiiiiii", style), width(("Inter", family), "MMMMMMMM", style));
            assert!((narrow - wide).abs() < 0.5, "{family} draws in a proportional fallback: {narrow} vs {wide}");
        }
    }

    #[test]
    fn the_system_interface_font_draws_in_its_own_face() {
        let style = TextStyle::new(14.0, 400.0);
        let [(inter, _), (system, _)] = super::INTERFACE;
        let sample = "Hamburgefonstiv";
        assert_ne!(width((inter, "Menlo"), sample, style), width((system, "Menlo"), sample, style), "System falls back to Inter");
    }

    #[test]
    fn a_family_that_isnt_installed_isnt_offered() {
        assert!(super::family_files("No Such Family").is_empty(), "Core Text must not substitute a missing family");
        assert!(super::offered("Menlo", true));
        assert!(!super::offered("No Such Family", true));
        assert!(!super::offered("Helvetica", true), "only the curated families");
        assert!(super::offered("System Font", false) && !super::offered("Menlo", false));
    }
}
