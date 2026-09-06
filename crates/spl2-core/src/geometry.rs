// SPDX-License-Identifier: GPL-2.0-only

//! Page geometry: the pure half of the 1.x filter, moved here unchanged so
//! both front ends compute identical numbers.
//!
//! Everything in this module was `src/main.rs` before the split, and the
//! diagnostic strings are kept verbatim: the 1.x filter is frozen (decision
//! Q-5), so a reworded message would be a behaviour change. New code added
//! during the split is written in English.
//!
//! The one structural change is the input type. The functions used to take
//! `raster::PageHeader`, which now lives behind the `golden-replay` feature;
//! they take [`PageGeometry`] instead, which both the CUPS filter and the
//! PAPPL raster callbacks can fill in.

use std::fmt;
use std::io;

use crate::qpdl::{self, SplDuplex, SplPaperSize, SplResolution};

/// Diagnostics sink; see [`crate::log`].
use crate::log::{Level, Log};

/// CUPS Renk Uzayı (`cups_cspace_e`) — ham sayısal kod.
///
/// Spesifikasyon 40'tan fazla renk uzayı tanımlar, ama bu sürücü yalnızca
/// `K` ile çalışır: diğer her değer `validate_page_header` tarafından
/// reddedilir. Bu yüzden uzayların tamamını ayrı ayrı modellemek yerine ham
/// kod saklanıyor. Karar veren iki nokta da (K kontrolü ve v2 çözücüsünün
/// boş renk dolgusu) zaten sayısal kodla çalışır; adlar yalnızca hata
/// mesajlarında görünür.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CupsColorSpace(pub u32);

impl CupsColorSpace {
    /// Siyah-tonlama (0 = siyah). Samsung lazer motorunun beklediği tek uzay.
    pub const K: CupsColorSpace = CupsColorSpace(3);

    /// `n == 128` (satır sonuna kadar boşalt) kaydında kullanılacak dolgu.
    ///
    /// libcups, toner/mürekkep EKLEYEN uzaylarda — K (3), CMY (4), CMYK (5),
    /// White (12), Gold (13), Silver (14) — boşluğu `0x00`, diğerlerinde
    /// `0xFF` ile doldurur.
    // Only the CUPS Raster line decoder needs this, and that lives behind
    // `golden-replay`.
    #[cfg_attr(not(feature = "golden-replay"), allow(dead_code))]
    pub(crate) fn blank_fill(self) -> u8 {
        match self.0 {
            3 | 4 | 5 | 12 | 13 | 14 => 0x00,
            _ => 0xFF,
        }
    }
}

impl fmt::Display for CupsColorSpace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let name = match self.0 {
            0 => "W (White=0 Grayscale)",
            1 => "RGB",
            2 => "RGBA",
            3 => "K (Black=0 Grayscale)",
            4 => "CMY",
            5 => "CMYK",
            18 => "sGray (sRGB Grayscale)",
            19 => "sRGB",
            20 => "AdobeRGB",
            other => return write!(f, "Bilinmeyen({})", other),
        };
        write!(f, "{}", name)
    }
}

/// CUPS Renk Dizilimi (`cups_order_e`)
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CupsColorOrder {
    /// Piksel baytları ardışık dizilir (Örn: RGBRGB... veya KKKK...)
    Chunked,
    /// Renk düzlemleri her satırda ayrı şeritler halindedir (RR... GG... BB...)
    Banded,
    /// Her renk düzlemi tüm sayfa boyunca ayrı bir sayfadır
    Planar,
    Unknown(u32),
}

impl From<u32> for CupsColorOrder {
    fn from(val: u32) -> Self {
        match val {
            0 => CupsColorOrder::Chunked,
            1 => CupsColorOrder::Banded,
            2 => CupsColorOrder::Planar,
            other => CupsColorOrder::Unknown(other),
        }
    }
}

/// Everything the QPDL engine needs to know about one page.
///
/// The 1.x filter passed `raster::PageHeader` around, which tied the protocol
/// engine to the classic CUPS Raster parser. This struct is the subset of that
/// header the engine actually reads, so the PAPPL raster callbacks can fill it
/// in from `pappl_pr_options_t` without a CUPS raster stream existing at all.
/// Field names and units are the CUPS ones, because every rule quoted in this
/// module is written against them.
#[derive(Debug, Clone)]
pub struct PageGeometry {
    /// `cupsWidth`, in pixels.
    pub width: u32,
    /// `cupsHeight`, in scanlines.
    pub height: u32,
    /// `cupsBytesPerLine`.
    pub bytes_per_line: u32,
    /// `HWResolution`, `[x, y]` in dpi.
    pub hw_resolution: [u32; 2],
    /// `PageSize`, `[width, length]` in points. This is the SHEET, not the
    /// printable area, and it selects the QPDL paper code.
    pub page_size_points: [u32; 2],
    /// `Margins`, `[left, bottom]` in points, as the raster stream declares
    /// them. Since 2026-09-06 this is validated but no longer drives
    /// placement; the driver constant does (`crate::media::HARD_MARGIN_PT`).
    pub margins: [u32; 2],
    /// `cupsBitsPerColor`.
    pub bits_per_color: u32,
    /// `cupsBitsPerPixel`.
    pub bits_per_pixel: u32,
    /// `cupsColorSpace`.
    pub color_space: CupsColorSpace,
    /// `cupsColorOrder`.
    pub color_order: CupsColorOrder,
    /// `NumCopies`, before [`sanitize_copies`].
    pub num_copies: u32,
    /// `MediaPosition`, the PPD `*InputSlot` code.
    pub media_position: u32,
    /// `Duplex`.
    pub duplex: bool,
    /// `Tumble` — the binding edge, not the QPDL page-side byte.
    pub tumble: bool,
    /// `MediaType`, mapped to a PJL `PAPERTYPE` at job level.
    pub media_type: String,
}

impl PageGeometry {
    /// Decoded raster volume for this page, in bytes; what [`JobBudget`]
    /// meters. Identical to the former `PageHeader::total_raster_bytes`.
    pub fn total_raster_bytes(&self) -> u64 {
        (self.bytes_per_line as u64) * (self.height as u64)
    }
}

/// Güvenilmez bir dizeyi, log satırına gömülmeye hazır hâle getirir.
///
/// `{:?}` (Debug) biçimi dizeyi tırnak içine alır ve kontrol karakterlerini
/// kaçırır (`\n`, `\u{1b}` gibi). Bu, iki saldırıyı birden kapatır:
/// gömülü bir CR/LF ile `/var/log/cups/error_log`'a sahte bir günlük satırı
/// enjekte etmek, ve gömülü ANSI/OSC dizileriyle logu izleyen yöneticinin
/// terminalini (renk, pencere başlığı) manipüle etmek.
///
/// Bu, `main`'in argv'den gelen `title`/`user` alanları için zaten uyguladığı
/// kalıbın aynısıdır; buradaki yardımcı, aynı politikayı raster başlığından
/// gelen dizeler ve dosya yolları için de tek bir yerde toplar.
pub fn quote_untrusted(value: &str) -> String {
    format!("{:?}", value)
}

/// Sayfa başlığı alanlarının makul sınırlar içinde olduğunu doğrular.
///
/// Bozuk ya da kötü niyetli bir CUPS Raster akışı aşırı büyük `bytesPerLine`,
/// yükseklik veya çözünürlük değerleri bildirebilir; bu değerler doğrudan
/// tampon boyutu hesaplarında kullanıldığından, doğrulanmadan geçirilmeleri
/// devasa/aşırı bellek tahsisine (OOM) ya da sessizce taşan hesaplamalara yol
/// açabilir. Bu sınırlar gerçekçi yazıcı donanımının çok üzerinde, sadece
/// açıkça saçma değerleri elemek için var.
/// PPD'deki en yüksek `*Resolution` seçeneği (1200 DPI).
pub const MAX_DPI: u32 = 1200;
/// En büyük `*PaperDimension` (Legal: 1008 pt) + makul pay.
pub const MAX_POINTS: u32 = 1300;
/// ~1300 pt * 1200 dpi / 72 / 8 ≈ 2709 B (1-bit); yuvarlanıp pay bırakıldı.
pub const MAX_BYTES_PER_LINE: u32 = 4096;
/// ~1300 pt * 1200 dpi / 72 ≈ 21.667 satır; yuvarlanıp pay bırakıldı.
pub const MAX_LINES: u32 = 24_000;

/// Fiziksel sayfa genişliğinin üzerinde kabul edilen yuvarlama payı.
/// Gerekçe için `validate_page_header` içindeki D-01 açıklamasına bakın.
pub const LINE_OVERSHOOT_SLACK_BYTES: u32 = 1;

/// Fiziksel sayfa yüksekliğinin üzerinde kabul edilen yuvarlama payı.
/// Gerekçe için `validate_page_header` içindeki D-02 açıklamasına bakın.
pub const HEIGHT_OVERSHOOT_SLACK_LINES: u32 = 8;

pub fn validate_page_geometry(header: &PageGeometry) -> io::Result<()> {
    // Sınırlar keyfi değil: ppd/samsung-ml2160.ppd'nin tanımladığı en yüksek
    // çözünürlükten ve en büyük kağıttan türetildi, makul bir pay bırakıldı.
    // Eski sınırlar (1.000.000 bayt/satır, 10.000 DPI, 100.000 pt) bu
    // donanımın fiziksel olarak üretebileceğinin ~100 katı üzerindeydi;
    // bozuk/kötü niyetli bir başlık bu boşluğu kullanıp devasa bant tamponları
    // (bkz. stream_page_bands) tahsis ettirebilirdi.
    //
    // PPD ile bu sabitler arasındaki bağ artık bir yorum değil, bir test:
    // `test_limits_cover_every_ppd_option` PPD'yi ayrıştırıp her `*Resolution`
    // ve `*PaperDimension` seçeneğinin sınırlar içinde kaldığını doğruluyor.
    // PPD'ye daha büyük bir kağıt ya da daha yüksek çözünürlük eklenirse test
    // kırılır ve sabitlerin birlikte güncellenmesi gerektiğini söyler.
    let invalid = |msg: String| Err(io::Error::new(io::ErrorKind::InvalidData, msg));

    if header.bytes_per_line == 0 || header.bytes_per_line > MAX_BYTES_PER_LINE {
        return invalid(format!(
            "Geçersiz cupsBytesPerLine değeri: {}",
            header.bytes_per_line
        ));
    }
    if header.height == 0 || header.height > MAX_LINES {
        return invalid(format!(
            "Geçersiz sayfa yüksekliği (satır sayısı): {}",
            header.height
        ));
    }
    if !SplResolution::pair_is_supported(header.hw_resolution[0], header.hw_resolution[1]) {
        return invalid(format!(
            "Desteklenmeyen çözünürlük: {}x{} DPI (desteklenenler: 300x300, 600x600, 1200x600, 1200x1200)",
            header.hw_resolution[0], header.hw_resolution[1]
        ));
    }
    if header.page_size_points[0] == 0
        || header.page_size_points[0] > MAX_POINTS
        || header.page_size_points[1] == 0
        || header.page_size_points[1] > MAX_POINTS
    {
        return invalid(format!(
            "Geçersiz sayfa boyutu (pt): {:?}",
            header.page_size_points
        ));
    }
    if SplPaperSize::from_dimensions_pt_exact(
        header.page_size_points[0],
        header.page_size_points[1],
    )
    .is_none()
    {
        return invalid(format!(
            "Desteklenmeyen kâğıt ölçüsü: {} x {} pt; QPDL kâğıt kodu ile raster geometrisinin uyuşması gerekir",
            header.page_size_points[0], header.page_size_points[1]
        ));
    }

    // D-07: `Margins[0]` da bir geometri alanıdır ve doğrulanmalıdır.
    //
    // Bu alan `hard_margin_bytes` üzerinden yatay yerleşimi belirler (bkz.
    // `band_placement`), ama bugüne kadar hiç denetlenmiyordu. Doğrulanmadan
    // geçen bir değerin iki başarısızlık kipi var: (1) devasa bir kenar
    // boşluğu (ör. 300.000.000 pt) `hard_margin_bytes` içindeki `px + 7`
    // toplamasını taşırır — `overflow-checks` açık yapılarda iş ortasında
    // panik, sürüm yapılarında sessizce 0'a sarma, yani kenar boşluğu
    // düzeltmesinin hiç uygulanmaması; (2) sayfadan geniş bir sol kenar
    // boşluğu fiziksel olarak anlamsızdır ve satırın tamamının atlanmasına
    // (boş sayfa) yol açar. Sol kenar boşluğu sayfanın kendisinden dar
    // olmalıdır; PPD'nin bütün `*ImageableArea` girdilerinde bu değer 12 pt'dir.
    // Sayfa genişliği yukarıda `MAX_POINTS` ile sınırlandığı için bu kontrol
    // aynı zamanda taşmayı da kapatır.
    if header.margins[0] >= header.page_size_points[0] {
        return invalid(format!(
            "Geçersiz sol kenar boşluğu: {} pt; sayfa genişliğinden ({} pt) küçük olmalı",
            header.margins[0], header.page_size_points[0]
        ));
    }

    // ML-2160 serisi QPDL motoru tek düzlemli, 1-bit monokrom (K) raster
    // bekler: stream_page_bands her baytı doğrudan tek bir siyah/beyaz
    // düzlem olarak yorumlayıp koşulsuz tersliyor (bkz. o fonksiyondaki
    // polarite açıklaması). Bu varsayımla uyuşmayan bir akış (ör. 24-bit RGB
    // ya da 32-bit CMYK) sessizce 1-bit monokrom sanılıp yazıcıya
    // gönderilirse şerit hizalaması bozulur, firmware senkronizasyonu
    // kaybolur ve gereksiz toner tüketimine yol açar; bu yüzden erken
    // reddediyoruz.
    if header.color_space != CupsColorSpace::K {
        return invalid(format!(
            "Desteklenmeyen renk uzayı: {} (yalnızca 1-bit K/monokrom destekleniyor)",
            header.color_space
        ));
    }
    if header.bits_per_color != 1 || header.bits_per_pixel != 1 {
        return invalid(format!(
            "Desteklenmeyen bit derinliği: bitsPerColor={}, bitsPerPixel={} (yalnızca 1-bit monokrom destekleniyor)",
            header.bits_per_color, header.bits_per_pixel
        ));
    }
    let expected_bytes_per_line = (header.width as u64 * header.bits_per_pixel as u64).div_ceil(8);
    if expected_bytes_per_line != header.bytes_per_line as u64 {
        return invalid(format!(
            "cupsBytesPerLine ({}) cupsWidth ({}) ile tutarsız (beklenen: {})",
            header.bytes_per_line, header.width, expected_bytes_per_line
        ));
    }

    // `cupsColorOrder` bugüne kadar hiç denetlenmiyordu. 1-bit tek kanallı
    // veride Chunked/Banded/Planar dizilimleri BİRBİRİNİN AYNIsıdır (tek
    // düzlem, tek kanal), bu yüzden üçü de kabul edilir; ama tanınmayan bir
    // değer, akışı üreten tarafın bu filtrenin varsaydığından farklı bir
    // düzen kullandığının işaretidir ve sessizce yanlış yorumlanmamalıdır.
    if let CupsColorOrder::Unknown(order) = header.color_order {
        return invalid(format!(
            "Tanınmayan cupsColorOrder değeri: {} (beklenen: 0=Chunked, 1=Banded, 2=Planar)",
            order
        ));
    }

    // D-01: `cupsBytesPerLine`, sayfanın FİZİKSEL genişliğine sığmalı.
    //
    // Bant genişliği `page_size_points` ve `hw_resolution`'dan hesaplanır
    // (bkz. compute_page_width_pixels); satır bundan genişse fazlalık
    // sessizce kırpılıyordu. Yukarıdaki `cupsWidth` tutarlılık kontrolü bunu
    // yakalamaz, çünkü kendi içinde tutarlı ama sayfayla tutarsız bir başlık
    // (ör. `PageSize = 1 pt` + `cupsBytesPerLine = 620`) bantı 1 bayta
    // düşürüp satırların %99,8'ini attırabiliyordu.
    //
    // Payın 1 bayt olmasının nedeni: `page_width_pixels` yukarı doğru 8'e
    // hizalandığı için normalde `bytes_per_line <= band_width_bytes` zaten
    // sağlanır; 1 baytlık pay yalnızca üretici tarafın farklı yuvarlaması
    // ihtimalini karşılar. Bu payın içinde kalan sapma, aşağıdaki sayfa
    // döngüsünde uyarıyla birlikte kırpılmaya devam eder.
    let band_width_bytes =
        compute_page_width_pixels(header.page_size_points[0], header.hw_resolution[0]).div_ceil(8);
    if header.bytes_per_line > band_width_bytes + LINE_OVERSHOOT_SLACK_BYTES {
        return invalid(format!(
            "cupsBytesPerLine ({}) sayfa genişliğine sığmıyor: {} pt @ {} DPI => en fazla {} bayt/satır",
            header.bytes_per_line,
            header.page_size_points[0],
            header.hw_resolution[0],
            band_width_bytes
        ));
    }

    // D-02: `cupsHeight`, sayfanın FİZİKSEL yüksekliğine sığmalı.
    //
    // D-01'in dikey karşılığı. Yükseklik bugüne kadar yalnızca global
    // `MAX_LINES` sınırına karşı denetleniyordu; sayfanın kendi boyutuyla hiç
    // karşılaştırılmıyordu. Kendi içinde tutarlı ama sayfayla tutarsız bir
    // başlık (ör. `PageSize = 595 x 1 pt` + `cupsHeight = 24000`) böylece
    // kabul ediliyor, QPDL sayfa başlığına 24000 satırlık bir yükseklik
    // yazılıyor ve sayfaya sığandan 3-4 kat fazla bant gönderiliyordu.
    // Yazıcıya bildirilen boyut ile gerçekte gönderilen veri miktarının
    // ayrışması, D-01'de olduğu gibi yazıcı tarafında hizalama/senkron kaybı
    // ve gereksiz kâğıt/toner tüketimi anlamına gelir.
    //
    // Payın 8 satır olmasının nedeni: genişlikten farklı olarak burada 8'e
    // hizalama YOK, yani `compute_page_height_lines` fazladan bir baş boşluk
    // bırakmıyor; pay yalnızca üretici tarafın farklı yuvarlaması (ceil yerine
    // round, ya da küçük bir bloğa hizalama) ihtimalini karşılıyor. Gerçek
    // cups-filters çıktısı bu sınırın çok altında kalır, çünkü PPD'nin
    // `*ImageableArea` kenar boşluklarını düşer: A4 @600 DPI'da fiziksel
    // 7017 satıra karşılık bu sistemde ölçülen `cupsHeight` 6817'dir (12 pt
    // üst + 12 pt alt yaklaşık 200 satır eksiltir). Yani meşru hiçbir iş bu
    // kontrole takılmaz.
    let page_height_lines =
        compute_page_height_lines(header.page_size_points[1], header.hw_resolution[1]);
    if header.height > page_height_lines + HEIGHT_OVERSHOOT_SLACK_LINES {
        return invalid(format!(
            "cupsHeight ({}) sayfa yüksekliğine sığmıyor: {} pt @ {} DPI => en fazla {} satır",
            header.height, header.page_size_points[1], header.hw_resolution[1], page_height_lines
        ));
    }

    Ok(())
}

/// SpliX document.cpp'deki `pageWidth` hesaplamasının Rust karşılığı.
///
/// SpliX kaynak kodu:
///   pageWidth = ((unsigned long)ceil(convertToXResolution(
///       request.printer()->pageWidth())) + 7) & ~7;
///
/// page_size_pt: Sayfa genişliği (1/72 inç, CUPS header.PageSize[0])
/// x_dpi: Yatay çözünürlük (CUPS header.HWResolution[0])
pub fn compute_page_width_pixels(page_size_pt: u32, x_dpi: u32) -> u32 {
    let px = (page_size_pt as f64 * x_dpi as f64 / 72.0).ceil() as u32;
    (px + 7) & !7u32
}

/// Sayfanın fiziksel yüksekliğinin kaç raster satırına karşılık geldiği.
///
/// `compute_page_width_pixels`'in dikey karşılığı, iki farkla: dikey eksende
/// bant/DMA hizalaması gerekmediği için 8'e yuvarlama YOKTUR, ve dikey
/// çözünürlük `hw_resolution[1]`'dir — PPD `1200x600dpi` gibi asimetrik bir
/// seçenek sunduğu için bu ayrım gerçekten tetiklenebilir.
///
/// Yalnızca `validate_page_header`'ın D-02 kontrolünde bir ÜST SINIR olarak
/// kullanılır; sayfa döngüsü satır sayısını (D-01'deki genişlik gibi) buna
/// göre yeniden ölçeklemez, çünkü gönderilecek satır sayısı `cupsHeight`
/// tarafından belirlenir.
pub fn compute_page_height_lines(page_size_pt: u32, y_dpi: u32) -> u32 {
    (page_size_pt as f64 * y_dpi as f64 / 72.0).ceil() as u32
}

/// Yazıcının SERT KENAR BOŞLUĞUNU (hard margin) bant tamponu baytına çevirir.
///
/// SpliX compress.cpp `_compressBandedPage`:
///
/// ```c
/// hardMarginX = ((unsigned long)ceil(page->convertToXResolution(
///     request.printer()->hardMarginX())) + 7) & ~7;
/// hardMarginXInB = hardMarginX / 8;
/// ```
///
/// The source is the driver constant, not integer CUPS Margins[]. This keeps
/// the selected 12.5 pt value intact: CUPS' integer field cannot represent it.
/// See docs/DECISIONS.md (2026-09-06). At 600 dpi it is 14 byte columns.
pub fn hard_margin_bytes(margin_pt: f64, x_dpi: u32) -> usize {
    let px = (margin_pt * x_dpi as f64 / 72.0).ceil() as u32;
    (((px + 7) & !7u32) / 8) as usize
}

/// The vertical counterpart of [`hard_margin_bytes`], in scanlines.
///
/// Two differences from the horizontal rule, and neither is cosmetic:
///
/// * **No 8-alignment.** `compute_page_height_lines` states the reason: the
///   band buffer is byte addressed horizontally, so the horizontal margin is
///   rounded up to a whole 8-pixel column, while the vertical axis addresses
///   scanlines individually and is not aligned.
/// * **`round`, not `ceil`.** The `ceil` in [`hard_margin_bytes`] costs
///   nothing, because the same rounded value is used on both sides of the
///   classic path's placement and cancels out of it. Vertically there is
///   nothing to cancel against: the 1.x filter never subtracts a vertical
///   margin at all, because cups-filters already centred the printable area on
///   the sheet — as `docs/MARGINS.md` puts it, centring and `hardMarginY`
///   "cancel exactly". So this value is not a matching convention but an
///   estimate of a physical distance, and the nearest scanline is the closest
///   the axis can come to it. At 600 dpi 12.5 pt is 104.17 lines: `round`
///   gives 104 and lands 0.17 lines low, `ceil` gives 105 and lands 0.83 lines
///   high, and 104 is also what cups-filters' own centring implies for the
///   classic path, so the two front ends stay within a scanline of each other.
///
/// Like every number on this path it is provisional until release gate G-1
/// measures a printed page; see `docs/GOLDEN-VALIDATION.md` and open question
/// Q-13 in `docs/DECISIONS.md`.
pub fn hard_margin_lines(margin_pt: f64, y_dpi: u32) -> u32 {
    (margin_pt * y_dpi as f64 / 72.0).round() as u32
}

/// CUPS satırının bant tamponundaki yatay yerleşimi.
///
/// İki alan birlikte tek bir işaretli ofseti temsil eder: `dst_offset`
/// pozitif kaydırma, `src_skip` ise negatif kaydırmadır (satırın solundan
/// atılan baytlar). İkisi aynı anda sıfırdan büyük olamaz.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BandPlacement {
    /// İçeriğin bant tamponunda başladığı sütun (bayt).
    pub dst_offset: usize,
    /// CUPS satırının başından atlanacak bayt sayısı.
    pub src_skip: usize,
}

/// CUPS satırının bant tamponundaki yatay konumunu SpliX ile aynı şekilde
/// hesaplar.
///
/// D-06 regresyonu. Burada eskiden yalnızca ORTALAMA vardı
/// (`(bandWidthInB - lineSize) / 2`) ve sert kenar boşluğu hiç düşülmüyordu;
/// oysa SpliX iki adımı da uygular:
///
/// ```c
/// // document.cpp:120 — satırı sayfa genişliğinde ortala
/// marginWidthInB = (pageWidthInB - lineSize) / 2;
/// // compress.cpp:227 — bandı doldururken sert kenar boşluğunu ATLA
/// band[x * bandHeight + y] = planes[i][index + x + hardMarginXInB + ...];
/// ```
///
/// Net ofset `ortalama - hardMarginXInB`'dir. A4 @600 DPI'da ortalama
/// `(620 - 595) / 2 = 12` bayt, sert kenar boşluğu 13 bayttır; yani içerik
/// bandın 0. sütunundan başlar. Yalnızca ortalama uygulandığında içerik 12
/// bayt (96 piksel ≈ 11,5 pt ≈ 4 mm) sağa kayıyordu ve sağ kenarı basılabilir
/// alanın dışına taşıyordu.
///
/// Bu, dikey eksenle de tutarlılık sağlar: dikeyde hiçbir zaman kaydırma
/// yapılmadı (sayfa döngüsü ilk satırı bandın 0. satırına yazar) ve SpliX'in
/// dikey neti de sıfırdır — ortalama `(7017 - 6817) / 2 = 100` satır, sert
/// kenar boşluğu `hardMarginY = 100` satır. İki eksenin farklı origin
/// varsayması hatanın kendisiydi.
pub fn band_placement(
    band_width_bytes: usize,
    cups_line_bytes: usize,
    hard_margin_bytes: usize,
) -> io::Result<BandPlacement> {
    let centered = band_width_bytes.saturating_sub(cups_line_bytes) / 2;
    let src_skip = hard_margin_bytes.saturating_sub(centered);

    // Satırın tamamının atlanması reddedilir. Burada eskiden bir kırpma vardı
    // (`.min(cups_line_bytes - 1)`) ve gerekçesi "boş bir sayfa üretilmesin"
    // diye yazılmıştı; oysa boş sayfayı üreten şeyin kendisi kırpmaydı: geriye
    // kalan tek bayt, satırdaki rastgele bir sütuna düşüyor ve sayfanın geri
    // kalanı sessizce kayboluyordu. Sert kenar boşluğunun satır genişliğini
    // aşması fiziksel olarak tutarsız bir geometridir; dosyanın geri kalanı
    // (bkz. D-01/D-02) böyle bir geometriyi sessizce düzeltmek yerine
    // reddettiği için burada da reddediyoruz.
    if src_skip >= cups_line_bytes {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!(
                "Sert kenar boşluğu ({} B) satır genişliğini ({} B) aşıyor: \
                 bant {} B, ortalama {} B; basılacak içerik kalmıyor",
                hard_margin_bytes, cups_line_bytes, band_width_bytes, centered
            ),
        ));
    }

    Ok(BandPlacement {
        dst_offset: centered.saturating_sub(hard_margin_bytes),
        src_skip,
    })
}

/// CUPS Raster sayfa başlığındaki duplex bilgisini QPDL duplex moduna çevirir.
///
/// SpliX `request.cpp` bu kararı PPD üzerinden verir:
///
/// ```c
/// manualDuplex = ppd->get("ManualDuplex", "QPDL").isTrue();
/// if (value == "DuplexNoTumble") _duplex = manualDuplex ? ManualLongEdge : LongEdge;
/// else if (value == "DuplexTumble") _duplex = manualDuplex ? ManualShortEdge : ShortEdge;
/// else _duplex = Simplex;
/// ```
///
/// PPD seçeneği (`DuplexNoTumble`/`DuplexTumble`) bize CUPS raster başlığındaki
/// `Duplex` + `Tumble` çifti olarak ulaşır: `Duplex=false` -> Simplex,
/// `Duplex=true, Tumble=false` -> uzun kenar, `Duplex=true, Tumble=true` ->
/// kısa kenar.
///
/// Bu projenin PPD'si de upstream SpliX'in kardeş model PPD'leri
/// (`ml1910.ppd`, `ml2010.ppd`, `ml2525.ppd`) gibi
/// `*QPDL ManualDuplex: "On"` bildiriyor ve ML-2160 serisinin
/// otomatik dupleks donanımı yok; dolayısıyla `manualDuplex` bu ailede her
/// zaman doğrudur ve sonuç daima `Manual*` varyantlarından biridir.
///
/// UYARI: bu yol ŞU AN ERİŞİLEMEZ. Projenin PPD'sinde `*OpenUI *Duplex` bloğu
/// bulunmadığı için CUPS raster başlığındaki `Duplex` hiçbir zaman set
/// edilmiyor. Eşleme yine de doğru tutuluyor ki PPD'ye duplex seçeneği
/// eklendiğinde protokol tarafı hazır olsun. Eklenmeden önce bilinmesi gereken
/// iki eksik için `stream_page_bands`'in altındaki nota bakın.
/// ELLE DUPLEX EKSİĞİ (2/2): elle duplex, işin iki geçişte basılmasını
/// gerektirir — önce bir yüz, sonra operatör kağıdı ters çevirip yeniden
/// yükledikten sonra diğer yüz. Bu filtre sayfaları akıştan geldikleri sırayla
/// tek geçişte gönderiyor; sayfa sırasını geçişlere bölmüyor. PPD'ye bir
/// `*OpenUI *Duplex` bloğu eklenecekse bu akışın da (ve yukarıdaki 1/2
/// maddesinin) çözülmesi gerekir, aksi hâlde çift taraflı işler yanlış sırada
/// basılır.
pub fn duplex_mode(duplex: bool, tumble: bool) -> SplDuplex {
    if !duplex {
        SplDuplex::Simplex
    } else if tumble {
        SplDuplex::ManualShortEdge
    } else {
        SplDuplex::ManualLongEdge
    }
}

/// CUPS Raster sayfa başlığındaki `MediaType` alanını yazıcının PJL
/// `PAPERTYPE` değerine çevirir; tanınmayan her değer `OFF`'a düşer.
///
/// Bu alan PPD'nin `*MediaType` seçeneğinden gelir (`<</MediaType(ENV)>>
/// setpagedevice` -> başlıkta `MediaType = "ENV"`) ve daha önce hiç
/// okunmuyordu: filtre her işte koşulsuz `@PJL SET PAPERTYPE=OFF` yazıyor,
/// yani zarf/etiket/kart stoğu seçen kullanıcı düz kağıt füzer ayarlarıyla
/// baskı alıyordu.
///
/// Geri düşüş SESSİZ değil: PPD ile filtrenin kelime dağarcığı ayrışırsa
/// (ör. eski, okunabilir adlar taşıyan bir PPD hâlâ kuruluysa) bu satır
/// kullanıcının seçiminin yazıcıya ulaşmadığını söyler.
pub fn pjl_paper_type_for(media_type: &str, log: &dyn Log) -> &'static str {
    if media_type.is_empty() {
        return qpdl::PJL_PAPERTYPE_DEFAULT;
    }
    match qpdl::pjl_paper_type(media_type) {
        Some(paper_type) => paper_type,
        None => {
            // `MediaType` 64 baytlık serbest bir C dizesidir ve işi gönderen
            // istemciden gelir; argv'deki `title`/`user` kadar güvenilmez,
            // bu yüzden kaçırılmış olarak basılır.
            log.log(
                Level::Warning,
                &format!(
                    "Tanınmayan MediaType {}; @PJL SET PAPERTYPE={} gönderiliyor. \
                     PPD'nin *MediaType anahtarları yazıcının PJL sözlüğünden olmalıdır: {}.",
                    quote_untrusted(media_type),
                    qpdl::PJL_PAPERTYPE_DEFAULT,
                    qpdl::PJL_PAPER_TYPES.join(", ")
                ),
            );
            qpdl::PJL_PAPERTYPE_DEFAULT
        }
    }
}

/// QPDL şerit (band) yüksekliğinin temel değeri, satır cinsinden.
///
/// SpliX bunu PPD'den okur (`*QPDL BandSize: "128"`); hem upstream SpliX'in
/// kardeş model PPD'leri (`ml1910.ppd`, `ml2010.ppd`, `ml2525.ppd`,
/// `ml1640.ppd`, `ml2510.ppd`) hem de bu projenin PPD'si 128 diyor.
pub const QPDL_BAND_HEIGHT: usize = 128;

/// Bir sayfa için kullanılacak şerit yüksekliği.
///
/// SpliX `compress.cpp` `_compressBandedPage` (Algo 0x11 bu yola gider; bkz.
/// aynı dosyadaki `compressPage` dağıtıcısı, 0x0D/0x0E/0x11 -> banded):
///
/// ```c
/// bandHeight = request.printer()->bandHeight();   // PPD: *QPDL BandSize
/// if (page->xResolution() == 300 && page->yResolution() == 300)
///     bandHeight /= 2;
/// ```
///
/// Yani 300x300 DPI'da şerit yüksekliği 128 değil 64'tür. Kural koşulsuzdur ve
/// üç yeri birden etkiler: bant tamponunun boyutu (`bandWidthInB * bandHeight`),
/// transpoze indeksleme (`band[x * bandHeight + y]`) ve şerit kaydına yazılan
/// yükseklik alanı. Bu filtre daha önce her çözünürlükte 128 kullanıyordu;
/// PPD'nin `300dpi` seçeneği seçildiğinde yazıcıya 64 satırlık şeritler
/// beklerken 128'e göre transpoze edilmiş veri gönderiliyordu.
///
/// Not: asimetrik `1200x600dpi` modu bu kuralın DIŞINDA kalır — koşul iki
/// eksenin de 300 olmasını istiyor — ve 128'de kalmaya devam eder.
pub fn band_height_for(hw_resolution: [u32; 2]) -> usize {
    if hw_resolution[0] == 300 && hw_resolution[1] == 300 {
        QPDL_BAND_HEIGHT / 2
    } else {
        QPDL_BAND_HEIGHT
    }
}

/// The QPDL band-order field is 8 bits wide, so a page may carry at most 256
/// bands; `write_compressed_band` fails the job rather than wrapping the
/// index. This assertion proves the job can never legitimately be refused for
/// that reason, from the validator's own limits rather than from the media
/// table:
///
/// * A page is at most `MAX_POINTS` tall and its vertical resolution at most
///   `MAX_DPI` (`validate_page_header`), so it carries at most
///   `MAX_POINTS * MAX_DPI / 72` lines — 21667, or 170 bands of 128.
/// * The halved band height is selected only when BOTH axes are 300 dpi, so
///   the 64-line band can only ever pair with a 300 dpi line count: 5417
///   lines, or 85 bands.
///
/// Both are inside the field with room to spare; the worst case reachable
/// from the PPD itself is Legal at 1200x1200, 129 bands
/// (`test_band_count_stays_inside_the_qpdl_band_order_field`). If the paper
/// table, the resolution list or `QPDL_BAND_HEIGHT` ever change enough to
/// break that, this fails at compile time instead of on paper.
const _: () = {
    const CEILING: usize = u8::MAX as usize + 1;
    // `usize::div_ceil` is not const, so the rounding is spelled out here.
    #[allow(clippy::manual_div_ceil)]
    const fn div_ceil(a: usize, b: usize) -> usize {
        (a + b - 1) / b
    }

    let lines_at_max_dpi = div_ceil(MAX_POINTS as usize * MAX_DPI as usize, 72);
    assert!(div_ceil(lines_at_max_dpi, QPDL_BAND_HEIGHT) <= CEILING);

    let lines_at_300_dpi = div_ceil(MAX_POINTS as usize * 300, 72);
    assert!(div_ceil(lines_at_300_dpi, QPDL_BAND_HEIGHT / 2) <= CEILING);
};

/// Tek bir baskı işinde işlenecek azami sayfa sayısı.
///
/// Sayfa döngüsünün üst sınırı yoktu: akış ne kadar uzunsa o kadar sayfa
/// üretiliyordu. Bu hem doğrudan kâğıt/toner tüketimini sınırsız bırakıyor
/// (`MAX_REALISTIC_COPIES` ile çarpıldığında daha da fazlası), hem de
/// sıkıştırıcının sayfa başına maliyetini toplamda sınırsız kılıyordu. Filtre
/// CUPS kuyruğunu tek iş parçacığıyla işlediği için uzun bir iş sıradaki tüm
/// işleri bekletir.
///
/// Sınır 5.000'den 1.000'e ÇEKİLDİ. Gerekçe, hedef donanımın kendisi: ML-2160
/// serisi ~20 sayfa/dakika basar, yani 1.000 sayfalık bir iş yazıcıyı zaten
/// ~50 dakika meşgul eder ve iki top kâğıt tüketir. 5.000 sayfa (~4 saat kesintisiz
/// baskı) bu sınıf bir kişisel yazıcıda gerçek bir belge değil, yalnızca
/// kötüye kullanım senaryosunun tavanıydı. Sınırı düşürmek, filtrenin en kötü
/// durumdaki CPU maliyetini de aynı oranda düşürür (bkz.
/// `MAX_JOB_RASTER_BYTES`).
pub const MAX_PAGES_PER_JOB: u32 = 1_000;

/// Tek bir baskı işinde işlenebilecek azami HAM RASTER hacmi (bayt).
///
/// `MAX_PAGES_PER_JOB` tek başına yetersiz, çünkü ÇÖZÜNÜRLÜK KÖRÜdür: bir
/// sayfanın işlenme maliyeti sayfa sayısıyla değil, bayt sayısıyla ölçeklenir.
/// Ölçülen değerler (bu makinede, release derlemesi, 346.752 baytlık gerçek
/// bant boyutunda):
///
/// * sıkıştırılabilir (sıfır dolu) bant: ~166 MB/s
/// * SIKIŞTIRILAMAZ gürültü: ~6,65 MB/s
///
/// Aradaki ~25 katlık fark, bayt cinsinden bir bütçenin CPU süresini ancak
/// kaba biçimde sınırlayabildiği anlamına gelir; bu yüzden bütçe, en kötü
/// durum kabul edilebilir kalacak şekilde seçilmelidir.
///
/// GİRDİ BOYUTUNDAN BAĞIMSIZLIK: bu bütçe ÇÖZÜLMÜŞ raster hacmini sayar,
/// girdi hacmini değil. CUPS Raster v2'nin satır-RLE'siyle, doğrulayıcının
/// kabul ettiği en büyük geometride (Legal @1200 DPI, yuvarlama paylarıyla
/// 1276 B/satır x 16.808 satır) yaklaşık 11.100 kat genişleme mümkündür.
/// Yaklaşık 0,74 MiB'lik tamamen beyaz bir akış bile 8 GiB'tan fazla raster
/// işi doğurabilir; tek gerçek savunma çözülen verinin tavanını doğrudan
/// sınırlamaktır.
///
/// 8 GiB, iki sınırın da anlamlı kalacağı şekilde seçildi:
///
/// * @600 DPI'da ölçülen A4 sayfa 595 x 6817 = ~3,87 MiB'dir;
///   `MAX_PAGES_PER_JOB` kadarı (1.000 sayfa) ~3,78 GiB eder, yani bütçenin
///   yarısının altında kalır. Sayfa sınırına kadar olan hiçbir normal
///   çözünürlüklü iş bu kontrole TAKILMAZ.
/// * En büyük kabul edilebilir sayfada (~20,45 MiB) bütçe 401. sayfada
///   devreye girer ve ölçülen en kötü sıkıştırma hızında işi yaklaşık 22
///   dakikayla sınırlar.
pub const MAX_JOB_RASTER_BYTES: u64 = 8 * 1024 * 1024 * 1024;

/// Tek bir baskı işinin üretebileceği azami YAPRAK sayısı (sayfa x kopya).
///
/// `MAX_PAGES_PER_JOB` ve `MAX_REALISTIC_COPIES` ayrı ayrı gerekçelendirilmişti
/// ama ÇARPIMLARI hiçbir yerde sınırlanmıyordu: eski değerlerle tek bir iş
/// 5.000 x 999 = 4.995.000 yaprak basma komutu üretebiliyordu. `num_copies`
/// sayfa başlığından, yani güvenilmez taraftan geldiği için kâğıt/toner
/// tüketimi açısından anlamlı olan tek sınır budur — sayfa sayısı değil,
/// yaprak sayısı.
///
/// 10.000 yaprak ~20 sayfa/dakikada ~8 saatlik kesintisiz baskıdır: hiçbir
/// meşru işin yaklaşamayacağı kadar cömert, ama milyonlarca yapraktan da
/// beş yüz kat uzak.
pub const MAX_JOB_IMPRESSIONS: u64 = 10_000;

/// Bir işin tükettiği kaynakların toplu muhasebesi.
///
/// Üç sayacın da tek bir yerde durmasının nedeni, birbirlerinin körlüğünü
/// kapatmaları: sayfa sayısı çözünürlük körü, raster hacmi kopya körü, yaprak
/// sayısı ise sayfa boyutu körüdür. Ayrı ayrı uygulandıklarında aralarındaki
/// çarpımsal boşluklar (bkz. `MAX_JOB_IMPRESSIONS`) gözden kaçıyordu.
#[derive(Debug, Default)]
pub struct JobBudget {
    pub pages: u32,
    pub raster_bytes: u64,
    pub impressions: u64,
}

impl JobBudget {
    /// Doğrulanmış bir sayfayı bütçeye işler ve sayfanın 1 TABANLI sırasını
    /// döner. Sınırlardan herhangi biri aşılırsa iş burada durur.
    ///
    /// `copies`, `sanitize_copies`'ten GEÇMİŞ değer olmalıdır: ham
    /// `num_copies` ile saymak, yazıcıya fiilen gönderilmeyecek kopyaları
    /// bütçeden düşerdi.
    ///
    /// Toplamalar `saturating_add` ile yapılıyor: sayfa başına hacim
    /// `validate_page_header` sayesinde ~59 MB ile, sayfa sayısı da
    /// `MAX_PAGES_PER_JOB` ile sınırlı olduğundan `u64` taşması zaten
    /// imkânsız — ama sınırlar değişirse sessizce sarmak yerine bütçeyi aşmış
    /// sayılması doğru davranıştır.
    pub fn account_page(&mut self, page_raster_bytes: u64, copies: u16) -> io::Result<u32> {
        let exceeded = |msg: String| io::Error::new(io::ErrorKind::InvalidData, msg);

        self.pages += 1;
        if self.pages > MAX_PAGES_PER_JOB {
            return Err(exceeded(format!(
                "İş, sayfa sınırını aştı: {} sayfadan fazlası işlenmiyor. \
                 Belge gerçekten bu kadar uzunsa işi parçalara bölün.",
                MAX_PAGES_PER_JOB
            )));
        }

        self.raster_bytes = self.raster_bytes.saturating_add(page_raster_bytes);
        if self.raster_bytes > MAX_JOB_RASTER_BYTES {
            return Err(exceeded(format!(
                "İş, ham raster hacmi sınırını aştı: {} bayttan fazlası işlenmiyor \
                 (şu ana kadar {} bayt). Belge gerçekten bu kadar büyükse işi \
                 parçalara bölün ya da daha düşük bir çözünürlük seçin.",
                MAX_JOB_RASTER_BYTES, self.raster_bytes
            )));
        }

        self.impressions = self.impressions.saturating_add(copies as u64);
        if self.impressions > MAX_JOB_IMPRESSIONS {
            return Err(exceeded(format!(
                "İş, yaprak sınırını aştı: {} yapraktan fazlası basılmıyor \
                 (şu ana kadar {} yaprak = sayfa x kopya). Kopya sayısını \
                 düşürün ya da işi parçalara bölün.",
                MAX_JOB_IMPRESSIONS, self.impressions
            )));
        }

        Ok(self.pages)
    }
}

/// Gerçekçi bir baskı işi için makul kabul edilen azami kopya sayısı.
///
/// QPDL'nin kopya alanı 16-bit'tir (teorik üst sınır 65535), ama hiçbir
/// gerçek iş bu sınıra yakın bir değer istemez; 999, kağıt/toner israfına
/// veya yazıcının fiziksel olarak saatlerce durmadan basmasına yol açacak
/// bozuk/aşırı bir başlığa karşı ek bir güvenlik payı bırakır.
pub const MAX_REALISTIC_COPIES: u16 = 999;

/// CUPS Raster başlığındaki `num_copies` (u32) alanını QPDL'nin 16-bit kopya
/// sayısı alanına güvenle sığacak şekilde normalize eder.
///
/// Önceki `header.num_copies.max(1) as u16` ifadesi, 65536 (2^16) ve katları
/// gibi değerlerde sessizce 0'a taşıyordu (`u16::MAX + 1 == 0`); bu da
/// yazıcıya fiilen "0 kopya bas" komutu gönderilmesine yol açardı.
/// `clamp(1, MAX_REALISTIC_COPIES)` hem alt hem üst sınırı aynı anda
/// garanti eder: 0 asla geçmez, aşırı büyük değerler ise sessizce taşmak
/// yerine (16-bit alana teknik olarak sığsa bile) gerçekçi bir üst sınıra
/// sabitlenir.
pub fn sanitize_copies(num_copies: u32) -> u16 {
    num_copies.clamp(1, MAX_REALISTIC_COPIES as u32) as u16
}
