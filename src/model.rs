//! The TI-84 family: which calculator is connected and what differs between models.
//!
//! Identification follows libticalcs: the low byte of parameter 0x0001 (product number) is
//! the product ID; 0x13 covers both the TI-84 Plus CE and the TI-83 Premium CE, told apart by
//! name. Models CalLink doesn't know (e.g. the 2026 TI-84 Evo) are still tried as DUSB devices.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Model {
    /// TI-84 Plus and TI-84 Plus Silver Edition (Z80, 96×64 mono)
    Ti84Plus,
    /// TI-84 Plus C Silver Edition (Z80, 320×240 colour, compressed screenshots)
    Ti84PlusCSE,
    /// TI-84 Plus CE, CE-T, CE Python, CE-T Python (eZ80, 320×240)
    Ti84PlusCE,
    /// TI-83 Premium CE (French CE)
    Ti83PremiumCE,
    /// TI-82 Advanced (Z80, mono)
    Ti82Advanced,
    /// TI-84 Plus T (Z80, mono)
    Ti84PlusT,
    Unknown(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OsKind {
    /// Intel-HEX flash pages, sent page by page (TI-84 Plus, C SE, 82 Advanced, 84 Plus T)
    Z80,
    /// Flat binary image (CE models)
    Ce,
}

pub const PRODUCT_TI84P: u8 = 0x0A;
pub const PRODUCT_TI82A: u8 = 0x0B;
pub const PRODUCT_TI84PCSE: u8 = 0x0F;
pub const PRODUCT_CE: u8 = 0x13;
pub const PRODUCT_TI84PT: u8 = 0x1B;

impl Model {
    /// From the product ID (low byte of parameter 0x0001) and the product name.
    pub fn identify(product_id: Option<u8>, name: &str) -> Model {
        let n = name.to_ascii_uppercase();
        match product_id {
            Some(PRODUCT_TI84P) => Model::Ti84Plus,
            Some(PRODUCT_TI82A) => Model::Ti82Advanced,
            Some(PRODUCT_TI84PCSE) => Model::Ti84PlusCSE,
            Some(PRODUCT_TI84PT) => Model::Ti84PlusT,
            Some(PRODUCT_CE) if n.contains("83") => Model::Ti83PremiumCE,
            Some(PRODUCT_CE) => Model::Ti84PlusCE,
            Some(id) => Model::Unknown(id),
            // no product number: go by name
            None if n.contains("PLUS C ") || n.contains("C SILVER") => Model::Ti84PlusCSE,
            None if n.contains("83 PREMIUM") => Model::Ti83PremiumCE,
            None if n.contains(" CE") => Model::Ti84PlusCE,
            None if n.contains("82 ADVANCED") => Model::Ti82Advanced,
            None if n.contains("84 PLUS T") => Model::Ti84PlusT,
            None if n.starts_with("TI-84 PLUS") => Model::Ti84Plus,
            None => Model::Unknown(0),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Model::Ti84Plus => "TI-84 Plus / Silver Edition",
            Model::Ti84PlusCSE => "TI-84 Plus C Silver Edition",
            Model::Ti84PlusCE => "TI-84 Plus CE",
            Model::Ti83PremiumCE => "TI-83 Premium CE",
            Model::Ti82Advanced => "TI-82 Advanced",
            Model::Ti84PlusT => "TI-84 Plus T",
            Model::Unknown(_) => "Unrecognised TI calculator",
        }
    }

    pub fn product_id(self) -> Option<u8> {
        Some(match self {
            Model::Ti84Plus => PRODUCT_TI84P,
            Model::Ti84PlusCSE => PRODUCT_TI84PCSE,
            Model::Ti84PlusCE | Model::Ti83PremiumCE => PRODUCT_CE,
            Model::Ti82Advanced => PRODUCT_TI82A,
            Model::Ti84PlusT => PRODUCT_TI84PT,
            Model::Unknown(0) => return None,
            Model::Unknown(id) => id,
        })
    }

    pub fn is_ce(self) -> bool {
        matches!(self, Model::Ti84PlusCE | Model::Ti83PremiumCE)
    }

    pub fn has_color_screen(self) -> bool {
        matches!(self, Model::Ti84PlusCSE | Model::Ti84PlusCE | Model::Ti83PremiumCE)
    }

    /// Simulated home screen size for the debugger (columns, rows).
    pub fn home_screen(self) -> (usize, usize) {
        if self.has_color_screen() { (26, 10) } else { (16, 8) }
    }

    pub fn os_kind(self) -> Option<OsKind> {
        match self {
            Model::Ti84Plus | Model::Ti84PlusCSE | Model::Ti82Advanced | Model::Ti84PlusT => Some(OsKind::Z80),
            Model::Ti84PlusCE | Model::Ti83PremiumCE => Some(OsKind::Ce),
            Model::Unknown(_) => None,
        }
    }

    /// OS file extension TI uses for this model.
    pub fn os_extension(self) -> &'static str {
        match self {
            Model::Ti84Plus | Model::Ti84PlusT => "8xu",
            Model::Ti84PlusCSE => "8cu",
            Model::Ti84PlusCE => "8eu",
            Model::Ti83PremiumCE => "8pu",
            Model::Ti82Advanced => "82u",
            Model::Unknown(_) => "8xu",
        }
    }

    /// Flash application extension, if the model runs apps.
    pub fn app_extension(self) -> Option<&'static str> {
        match self {
            Model::Ti84Plus => Some("8xk"),
            Model::Ti84PlusCSE => Some("8ck"),
            Model::Ti84PlusCE | Model::Ti83PremiumCE => Some("8ek"),
            _ => None,
        }
    }

    /// Format an OS/boot version as TI writes it for this model:
    /// CE `00 05 08 03` = 5.8.3; Z80 models `00 02 37 00` = 2.55, C SE `00 04 14 00` = 4.20.
    pub fn format_version(self, d: &[u8]) -> String {
        match d {
            [0, a, b, c] if self.is_ce() || (matches!(self, Model::Unknown(_)) && *a >= 5) => format!("{a}.{b}.{c}"),
            [0, a, b, 0] => format!("{a}.{b:02}"),
            [0, a, b, c] => format!("{a}.{b:02} (build {c})"),
            [a, b, c, e] => format!("{a}.{b}.{c}.{e:04}"),
            [a, b, c, e, f] => format!("{a}.{b}.{c}.{:04}", u16::from_be_bytes([*e, *f])),
            _ => d.iter().map(|b| b.to_string()).collect::<Vec<_>>().join("."),
        }
    }

    /// TI's web page for this model's OS, when there is a known one.
    pub fn os_page(self, product_name: &str) -> Option<&'static str> {
        let n = product_name.to_ascii_uppercase();
        Some(match self {
            Model::Ti84Plus => "https://education.ti.com/en/software/details/en/B7DADA7FD4AA40CE9D7911B004B8C460/ti84plusoperatingsystem",
            Model::Ti84PlusCSE => "https://education.ti.com/en/software/details/en/5F0CBAC101194542B16B80BCE6CB3602/ti-84-plus-c-silver-edition-operating-system",
            Model::Ti84PlusCE if n.contains("CE-T") => "https://education.ti.com/en/software/details/en/D278A0ACF2D448BC8FC2D94EF50DD8FD/TI-84_Plus_CE_Python_OS",
            Model::Ti84PlusCE if n.contains("PYTHON") => "https://education.ti.com/en/software/details/en/4505D98069044C2099171B3BEC63E5B3/TI-84_Plus_CE_Python_OS",
            Model::Ti84PlusCE => "https://education.ti.com/en/software/details/en/158B7669E4C0493A84D33D9A22FDBD3C/ti-84plusceoperatingsystem",
            _ => return None,
        })
    }
}

/// Picture files are .8xi on monochrome models and .8ci on colour ones; tell them apart by size
/// (a monochrome picture is 756 bytes: 96×63 pixels, 1 bit each).
pub fn picture_extension(data_len: usize) -> &'static str {
    if data_len <= 768 { "8xi" } else { "8ci" }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identify_models() {
        assert_eq!(Model::identify(Some(0x0A), "TI-84 Plus"), Model::Ti84Plus);
        assert_eq!(Model::identify(Some(0x13), "TI-84 Plus CE"), Model::Ti84PlusCE);
        assert_eq!(Model::identify(Some(0x13), "TI-83 Premium CE"), Model::Ti83PremiumCE);
        assert_eq!(Model::identify(Some(0x0F), "TI-84 Plus C Silver Edition"), Model::Ti84PlusCSE);
        assert_eq!(Model::identify(Some(0x1B), "TI-84 Plus T"), Model::Ti84PlusT);
        assert_eq!(Model::identify(Some(0x0B), "TI-82 Advanced"), Model::Ti82Advanced);
        assert_eq!(Model::identify(Some(0x42), "TI-84 Evo"), Model::Unknown(0x42));
        assert_eq!(Model::identify(None, "TI-84 Plus C Silver Edition"), Model::Ti84PlusCSE);
    }

    #[test]
    fn versions_as_ti_writes_them() {
        assert_eq!(Model::Ti84Plus.format_version(&[0, 2, 0x37, 0]), "2.55");
        assert_eq!(Model::Ti84PlusCE.format_version(&[0, 5, 8, 3]), "5.8.3");
        assert_eq!(Model::Ti84PlusCSE.format_version(&[0, 4, 20, 0]), "4.20");
        assert_eq!(Model::Ti84PlusCE.format_version(&[0, 5, 6, 1]), "5.6.1");
    }

    #[test]
    fn extensions() {
        assert_eq!(Model::Ti84PlusCSE.os_extension(), "8cu");
        assert_eq!(Model::Ti83PremiumCE.os_extension(), "8pu");
        assert_eq!(picture_extension(756), "8xi");
        assert_eq!(picture_extension(21945), "8ci");
    }
}
