use gpui::{Hsla, Image};
use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

/// An icon from the system's SF Symbols catalog.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub enum Symbol {
    Files,
    Commits,
    SideBySide,
    Inline,
    Refresh,
    Finish,
    Copy,
    Previous,
    Next,
    Collapsed,
    Expanded,
}

impl Symbol {
    #[cfg(target_os = "macos")]
    fn system_name(self) -> &'static str {
        match self {
            Self::Files => "doc.text",
            Self::Commits => "arrow.triangle.branch",
            Self::SideBySide => "rectangle.split.2x1",
            Self::Inline => "text.justify.left",
            Self::Refresh => "arrow.clockwise",
            Self::Finish => "checkmark.circle",
            Self::Copy => "doc.on.clipboard",
            Self::Previous => "chevron.left",
            Self::Next => "chevron.right",
            Self::Collapsed => "chevron.right",
            Self::Expanded => "chevron.down",
        }
    }
}

type TintedSymbol = (Symbol, [u8; 4]);

/// Symbol images tinted on demand; empty off macOS, where the buttons show text alone.
#[derive(Default)]
pub struct Symbols {
    images: RefCell<HashMap<TintedSymbol, Option<Arc<Image>>>>,
}

impl Symbols {
    pub fn image(&self, symbol: Symbol, color: Hsla) -> Option<Arc<Image>> {
        let rgba = color.to_rgb();
        let key = [rgba.r, rgba.g, rgba.b, rgba.a].map(|channel| (channel * 255.) as u8);
        self.images
            .borrow_mut()
            .entry((symbol, key))
            .or_insert_with(|| Self::rasterize(symbol, color))
            .clone()
    }

    #[cfg(target_os = "macos")]
    fn rasterize(symbol: Symbol, color: Hsla) -> Option<Arc<Image>> {
        use gpui::ImageFormat;
        use objc2_app_kit::{
            NSBitmapImageFileType, NSBitmapImageRep, NSColor, NSFontWeightRegular, NSImage,
            NSImageSymbolConfiguration,
        };
        use objc2_foundation::{NSDictionary, NSString};
        // Twice the on-screen size, so it stays sharp on Retina displays.
        const POINTS: f64 = 28.;

        let name = NSString::from_str(symbol.system_name());
        let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(&name, None)?;
        let rgba = color.to_rgb();
        let tint = NSColor::colorWithSRGBRed_green_blue_alpha(
            rgba.r.into(),
            rgba.g.into(),
            rgba.b.into(),
            rgba.a.into(),
        );
        let size = NSImageSymbolConfiguration::configurationWithPointSize_weight(POINTS, unsafe {
            NSFontWeightRegular
        });
        let tint = NSImageSymbolConfiguration::configurationWithHierarchicalColor(&tint);
        let image = image
            .imageWithSymbolConfiguration(&size.configurationByApplyingConfiguration(&tint))?;
        let tiff = image.TIFFRepresentation()?;
        let bitmap = NSBitmapImageRep::imageRepWithData(&tiff)?;
        let png = unsafe {
            bitmap.representationUsingType_properties(
                NSBitmapImageFileType::PNG,
                &NSDictionary::new(),
            )
        }?;
        Some(Arc::new(Image::from_bytes(ImageFormat::Png, png.to_vec())))
    }

    #[cfg(not(target_os = "macos"))]
    fn rasterize(_: Symbol, _: Hsla) -> Option<Arc<Image>> {
        None
    }
}
