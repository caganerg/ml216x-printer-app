// SPDX-License-Identifier: GPL-2.0-only

//! The driver capability table, and the three mappings that connect IPP
//! vocabulary to the QPDL vocabulary the engine speaks.
//!
//! `docs/MIGRATION-PLAN.md` §7 requires these to be checked against
//! `ppd/samsung-ml2160.ppd` rather than merely derived from it once, so the
//! PPD does not become an unmaintained second source of truth. The tests at
//! the bottom of this file do that.

use pappl::application::Media;

/// One medium, in all three vocabularies.
pub struct Medium {
    /// PWG self-describing name and physical size, in hundredths of a
    /// millimetre, as PAPPL and IPP want it.
    pub pwg: Media,
    /// The `*PageSize` key in the classic PPD. Only the tests read it, and
    /// that is the point: it is what ties this table back to the PPD.
    #[cfg_attr(not(test), allow(dead_code))]
    pub ppd_key: &'static str,
    /// `*PaperDimension`, in points. This is what selects the QPDL paper code
    /// and sizes the band buffer, so the 2.0 path must keep using the same
    /// rounded points the frozen filter uses, not the exact PWG millimetres.
    pub points: [u32; 2],
}

pub const MEDIA_TABLE: &[Medium] = &[
    Medium {
        pwg: Media {
            name: c"iso_a4_210x297mm",
            width: 21000,
            length: 29700,
        },
        ppd_key: "A4",
        points: [595, 842],
    },
    Medium {
        pwg: Media {
            name: c"na_letter_8.5x11in",
            width: 21590,
            length: 27940,
        },
        ppd_key: "Letter",
        points: [612, 792],
    },
    Medium {
        pwg: Media {
            name: c"na_legal_8.5x14in",
            width: 21590,
            length: 35560,
        },
        ppd_key: "Legal",
        points: [612, 1008],
    },
    Medium {
        pwg: Media {
            name: c"na_executive_7.25x10.5in",
            width: 18415,
            length: 26670,
        },
        ppd_key: "Executive",
        points: [522, 756],
    },
    Medium {
        pwg: Media {
            name: c"iso_a5_148x210mm",
            width: 14800,
            length: 21000,
        },
        ppd_key: "A5",
        points: [420, 595],
    },
    Medium {
        pwg: Media {
            name: c"iso_a6_105x148mm",
            width: 10500,
            length: 14800,
        },
        ppd_key: "A6",
        points: [297, 420],
    },
    Medium {
        pwg: Media {
            name: c"jis_b5_182x257mm",
            width: 18200,
            length: 25700,
        },
        ppd_key: "B5",
        points: [516, 729],
    },
    Medium {
        pwg: Media {
            name: c"na_number-10_4.125x9.5in",
            width: 10477,
            length: 24130,
        },
        ppd_key: "Env10",
        points: [297, 684],
    },
    Medium {
        pwg: Media {
            name: c"iso_dl_110x220mm",
            width: 11000,
            length: 22000,
        },
        ppd_key: "EnvDL",
        points: [312, 624],
    },
    Medium {
        pwg: Media {
            name: c"iso_c5_162x229mm",
            width: 16200,
            length: 22900,
        },
        ppd_key: "EnvC5",
        points: [459, 649],
    },
    Medium {
        pwg: Media {
            name: c"om_folio_210x330mm",
            width: 21000,
            length: 33000,
        },
        ppd_key: "Folio",
        points: [595, 935],
    },
];

const MEDIA_COUNT: usize = MEDIA_TABLE.len();

const MEDIA_ARRAY: [Media; MEDIA_COUNT] = {
    let mut out = [MEDIA_TABLE[0].pwg; MEDIA_COUNT];
    let mut i = 0;
    while i < MEDIA_COUNT {
        out[i] = MEDIA_TABLE[i].pwg;
        i += 1;
    }
    out
};

/// What the capability table publishes to IPP.
pub const MEDIA: &[Media] = &MEDIA_ARRAY;

/// Input slots, in the order their QPDL codes run.
///
/// The code is the CUPS `MediaPosition` value the classic path carried; see
/// `SplPaperSource::from_media_position`.
pub const SOURCES: &[(&std::ffi::CStr, u32)] = &[(c"auto", 1), (c"manual", 2)];

const SOURCE_ARRAY: [&std::ffi::CStr; 2] = [SOURCES[0].0, SOURCES[1].0];

/// What the capability table publishes to IPP.
pub const SOURCE_NAMES: &[&std::ffi::CStr] = &SOURCE_ARRAY;

/// IPP `media-type` keyword to the printer's PJL `PAPERTYPE` word.
///
/// The PJL words are the PPD's `*MediaType` keys, which are the printer's own
/// vocabulary. The IPP keywords are the registered names for the same stocks;
/// `other` is the only positional pair rather than a semantic one — it takes
/// the remaining PJL word, `ARCHIVE`, which IPP has no keyword for.
pub const MEDIA_TYPES: &[(&std::ffi::CStr, &str)] = &[
    (c"auto", "OFF"),
    (c"stationery", "NORMAL"),
    (c"stationery-heavyweight", "THICK"),
    (c"stationery-lightweight", "THIN"),
    (c"stationery-bond", "BOND"),
    (c"transparency", "OHP"),
    (c"cardstock", "CARD"),
    (c"labels", "LABEL"),
    (c"stationery-preprinted", "USED"),
    (c"stationery-colored", "COLOR"),
    (c"envelope", "ENV"),
    (c"stationery-cotton", "COTTON"),
    (c"stationery-recycled", "RECYCLED"),
    (c"other", "ARCHIVE"),
];

const TYPE_COUNT: usize = MEDIA_TYPES.len();

const TYPE_ARRAY: [&std::ffi::CStr; TYPE_COUNT] = {
    let mut out = [MEDIA_TYPES[0].0; TYPE_COUNT];
    let mut i = 0;
    while i < TYPE_COUNT {
        out[i] = MEDIA_TYPES[i].0;
        i += 1;
    }
    out
};

/// What the capability table publishes to IPP.
pub const TYPE_NAMES: &[&std::ffi::CStr] = &TYPE_ARRAY;

/// The classic PPD point dimensions for a PWG medium.
pub fn legacy_points(pwg_name: &str) -> Option<[u32; 2]> {
    MEDIA_TABLE
        .iter()
        .find(|m| m.pwg.name.to_bytes() == pwg_name.as_bytes())
        .map(|m| m.points)
}

/// The QPDL tray code for an IPP `media-source` keyword.
pub fn media_position(source: &str) -> Option<u32> {
    SOURCES
        .iter()
        .find(|(name, _)| name.to_bytes() == source.as_bytes())
        .map(|(_, code)| *code)
}

/// The PJL `PAPERTYPE` word for an IPP `media-type` keyword.
pub fn pjl_media_type(media_type: &str) -> Option<&'static str> {
    MEDIA_TYPES
        .iter()
        .find(|(name, _)| name.to_bytes() == media_type.as_bytes())
        .map(|(_, pjl)| *pjl)
}

#[cfg(test)]
mod tests {
    use super::*;
    use spl2_core::qpdl::{SplPaperSource, PJL_PAPER_TYPES};
    use std::fs;

    fn ppd() -> String {
        fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../ppd/samsung-ml2160.ppd"
        ))
        .expect("could not read the PPD")
    }

    /// The 2.0 capability table must describe the same sheets the frozen
    /// filter's PPD does, or the two paths would put a page of one size in a
    /// band buffer sized for another.
    #[test]
    fn every_medium_matches_the_ppd_paper_dimension() {
        let ppd = ppd();
        for medium in MEDIA_TABLE {
            let line = format!("*PaperDimension {}/", medium.ppd_key);
            let found = ppd
                .lines()
                .find(|l| l.starts_with(&line))
                .unwrap_or_else(|| panic!("no *PaperDimension {} in the PPD", medium.ppd_key));
            let values: Vec<u32> = found
                .split('"')
                .nth(1)
                .expect("*PaperDimension value must be quoted")
                .split_whitespace()
                .map(|v| v.parse().expect("*PaperDimension must be numeric"))
                .collect();
            assert_eq!(
                values,
                medium.points.to_vec(),
                "{} points disagree with the PPD",
                medium.ppd_key
            );
        }
        let ppd_sizes = ppd
            .lines()
            .filter(|l| l.starts_with("*PaperDimension "))
            .count();
        assert_eq!(
            ppd_sizes,
            MEDIA_TABLE.len(),
            "the PPD and the capability table list different numbers of media"
        );
    }

    /// The PWG size and the PPD points describe the same sheet: points are
    /// rounded millimetres, so they must agree to within one point.
    #[test]
    fn pwg_millimetres_and_ppd_points_describe_the_same_sheet() {
        for medium in MEDIA_TABLE {
            for (hundredths_mm, points) in [
                (medium.pwg.width, medium.points[0]),
                (medium.pwg.length, medium.points[1]),
            ] {
                let from_mm = f64::from(hundredths_mm) * 72.0 / 2540.0;
                let delta = (from_mm - f64::from(points)).abs();
                assert!(
                    delta <= 1.0,
                    "{}: {} hundredths mm is {:.2} pt, table says {} pt",
                    medium.ppd_key,
                    hundredths_mm,
                    from_mm,
                    points
                );
            }
        }
    }

    /// Every PJL paper type the printer knows must be reachable from IPP, and
    /// every keyword we publish must map to one the engine accepts.
    #[test]
    fn media_types_cover_the_printers_whole_vocabulary() {
        for (keyword, pjl) in MEDIA_TYPES {
            assert!(
                PJL_PAPER_TYPES.contains(pjl),
                "{:?} maps to {:?}, which the printer does not know",
                keyword,
                pjl
            );
        }
        for pjl in PJL_PAPER_TYPES {
            assert!(
                MEDIA_TYPES.iter().any(|(_, mapped)| mapped == &pjl),
                "PJL paper type {pjl:?} is not reachable from any IPP keyword"
            );
        }
        assert_eq!(MEDIA_TYPES.len(), PJL_PAPER_TYPES.len());
    }

    /// The PPD's `*MediaType` keys are the PJL words; if the PPD gains a stock
    /// the table has to gain a keyword for it.
    #[test]
    fn media_types_match_the_ppd_keys() {
        let ppd = ppd();
        let keys: Vec<&str> = ppd
            .lines()
            .filter_map(|l| l.strip_prefix("*MediaType "))
            .filter_map(|l| l.split('/').next())
            .collect();
        assert_eq!(keys.len(), MEDIA_TYPES.len());
        for key in keys {
            assert!(
                MEDIA_TYPES.iter().any(|(_, pjl)| *pjl == key),
                "the PPD offers *MediaType {key} and no IPP keyword maps to it"
            );
        }
    }

    /// The tray codes must be the ones the engine decodes.
    #[test]
    fn sources_map_to_the_engines_tray_codes() {
        for (name, code) in SOURCES {
            let source = SplPaperSource::from_media_position(*code)
                .unwrap_or_else(|| panic!("{name:?} maps to unknown MediaPosition {code}"));
            let expected = match name.to_bytes() {
                b"auto" => SplPaperSource::Auto,
                b"manual" => SplPaperSource::Manual,
                other => panic!("unhandled source {:?}", String::from_utf8_lossy(other)),
            };
            assert_eq!(source, expected);
        }
    }

    #[test]
    fn lookups_agree_with_the_table() {
        assert_eq!(legacy_points("iso_a4_210x297mm"), Some([595, 842]));
        assert_eq!(legacy_points("iso_a4_210x297"), None);
        assert_eq!(media_position("manual"), Some(2));
        assert_eq!(media_position("tray-4"), None);
        assert_eq!(pjl_media_type("envelope"), Some("ENV"));
        assert_eq!(pjl_media_type("ENV"), None);
        assert_eq!(MEDIA.len(), MEDIA_TABLE.len());
        assert_eq!(TYPE_NAMES.len(), MEDIA_TYPES.len());
    }
}
