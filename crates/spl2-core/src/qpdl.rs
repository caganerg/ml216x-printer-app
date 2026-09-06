//! # Samsung Printer Language (SPL2 / QPDL v3) Protokol Modülü
//!
//! Samsung ML-2160 serisi ve uyumlu monokrom QPDL/SPL2 lazer yazıcılar için
//! tam uyumlu PJL iş kontrolü, 17-baytlık sayfa başlığı, 0x09ABCDEF alt-başlıklı
//! ve sağlama toplamlı (checksum) Algo 0x11 RLE şerit kodlaması.
//!
//! Kaynak: OpenPrinting SpliX (QPDL v3 / ML-2160 serisi)
//! Lisans: GPLv2 (yalnızca v2 — SpliX kaynağıyla aynı)

use std::io::{self, Write};
use std::time::{SystemTime, UNIX_EPOCH};

/// PJL Universal Exit Language (UEL)
pub const PJL_UEL: &[u8] = b"\x1b%-12345X";
pub const PJL_END: &[u8] = b"\t\x1b%-12345X";

/// Alt Başlık İmzası (Sub-header Signature: 0x09ABCDEF - Little Endian)
pub const SUBHEADER_SIG_LE: [u8; 4] = [0xEF, 0xCD, 0xAB, 0x09];

/// Algo 0x11 RLE Sıkıştırma Sabitleri
pub const COMPRESS_SAMPLE_RATE: usize = 0x800; // 2048 bayt
pub const TABLE_PTR_SIZE: usize = 0x40; // 64 adet işaretçi/ofset
pub const MAX_UNCOMPRESSED_BYTES: usize = 0x80; // 128 bayt
pub const MIN_COMPRESSED_BYTES: usize = 2; // > 2 bayt (en az 3 bayt eşleşme)
pub const MAX_COMPRESSED_BYTES: usize = 0x1FF + 3; // 514 bayt
pub const COMPRESSION_FLAG: u8 = 0x80;

/// Samsung QPDL Kağıt Boyutu Tanımları
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum SplPaperSize {
    Letter = 0,
    Legal = 1,
    #[default]
    A4 = 2,
    Executive = 3,
    Ledger = 4,
    A3 = 5,
    Env10 = 6,
    Monarch = 7,
    C5 = 8,
    Dl = 9,
    B4 = 10,
    B5 = 11,
    EnvIsoB5 = 12,
    Postcard = 14,
    A5 = 16,
    A6 = 17,
    B6 = 18,
    Custom = 21,
    C6 = 23,
    Folio = 24,
    EnvPersonal = 25,
    Env9 = 26,
    Oficio = 28,
}

impl SplPaperSize {
    /// Fiziksel ölçüyü TANINAN bir QPDL kağıt koduna eşler; tablo dışındaki
    /// her ölçü için `None` döner.
    ///
    /// Bilinmeyen bir ölçü başka bir kâğıt koduna düşürülmez: QPDL sayfa
    /// başlığındaki kâğıt kodu ile gönderilen piksel geometrisinin ayrışması
    /// yazıcı tarafında hizalama ve besleme hatalarına yol açabilir. Çağıran
    /// taraf `None` sonucunu işi reddederek ele almalıdır.
    pub fn from_dimensions_pt_exact(width_pt: u32, height_pt: u32) -> Option<Self> {
        Some(match (width_pt, height_pt) {
            (595, 842) | (842, 595) => SplPaperSize::A4,
            (612, 792) | (792, 612) => SplPaperSize::Letter,
            (612, 1008) | (1008, 612) => SplPaperSize::Legal,
            (420, 595) | (595, 420) => SplPaperSize::A5,
            (297, 420) | (420, 297) => SplPaperSize::A6,
            (522, 756) | (756, 522) => SplPaperSize::Executive,
            // Folio (F4) = 210x330 mm = 595.28 x 935.43 pt -> 595 x 935.
            // Daha önce burada 612 x 936 vardı, ki o Folio değil 8.5x13 inç,
            // yani Adobe adlandırmasıyla FanFoldGermanLegal'dir — `cupstestppd`
            // de PPD'yi tam bu gerekçeyle uyarıyordu ("Size "Folio" should be
            // the Adobe standard name "FanFoldGermanLegal""). Upstream
            // SpliX'in aynı motor ailesine ait PPD'leri (ml1910.ppd,
            // ml2010.ppd, ml2525.ppd, ml1640.ppd, ml2510.ppd) da
            // `*PaperDimension Folio: "595 935"` diyor. DİKKAT: SpliX'te
            // ml2160.ppd/ml2165.ppd diye bir dosya YOKTUR; bu seri upstream
            // PPD listesinde yer almaz, bu yüzden referans olarak aynı QPDL
            // v3 protokolünü konuşan kardeş modeller kullanılıyor.
            (595, 935) | (935, 595) => SplPaperSize::Folio,
            (516, 729) | (729, 516) => SplPaperSize::B5,
            (297, 684) | (684, 297) => SplPaperSize::Env10,
            (312, 624) | (624, 312) => SplPaperSize::Dl,
            (459, 649) | (649, 459) => SplPaperSize::C5,
            _ => return None,
        })
    }
}

/// Kağıt Kaynağı (Tray)
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum SplPaperSource {
    #[default]
    Auto = 1,
    Manual = 2,
    Multi = 3,
    Upper = 4,
    Lower = 5,
}

impl SplPaperSource {
    /// CUPS Raster başlığındaki `MediaPosition` alanını QPDL kağıt kaynağı
    /// koduna eşler; tanınmayan her değer için `None` döner.
    ///
    /// Eşleme birebirdir, çünkü bu projenin PPD'si `*InputSlot` seçeneklerini
    /// doğrudan QPDL kodlarıyla numaralandırır (`<</MediaPosition 1>>` = Auto,
    /// `<</MediaPosition 2>>` = Manual). Aynı numaralandırmayı upstream
    /// SpliX'in ml1910.ppd / ml2010.ppd / ml2525.ppd / ml1640.ppd /
    /// ml2510.ppd dosyaları da kullanır. Bağı bir yorum olarak bırakmamak
    /// için `test_every_ppd_input_slot_maps_to_its_qpdl_code` PPD'yi
    /// ayrıştırıp her seçeneğin beklenen koda düştüğünü doğrular.
    ///
    /// `0`, "hiçbir kaynak seçilmedi" demektir (PPD'siz üretilmiş bir raster,
    /// ör. `cupsfilter -p` olmadan) ve sessizce `Auto`'ya düşer — bu bir
    /// sapma değil, alanın yokluğudur. Tanınmayan DİĞER değerler `None`
    /// döner ki çağıran taraf uyarabilsin.
    pub fn from_media_position(media_position: u32) -> Option<Self> {
        Some(match media_position {
            0 | 1 => SplPaperSource::Auto,
            2 => SplPaperSource::Manual,
            3 => SplPaperSource::Multi,
            4 => SplPaperSource::Upper,
            5 => SplPaperSource::Lower,
            _ => return None,
        })
    }
}

/// Çözünürlük
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SplResolution {
    Dpi300,
    #[default]
    Dpi600,
    Dpi1200,
}

impl SplResolution {
    #[inline]
    pub fn dpi(&self) -> u32 {
        match self {
            SplResolution::Dpi300 => 300,
            SplResolution::Dpi600 => 600,
            SplResolution::Dpi1200 => 1200,
        }
    }

    /// Bir DPI değerini yalnızca QPDL'nin bu sürücüde desteklediği kesin
    /// değerlerden biriyse dönüştürür. Yakındaki değere yuvarlama yapılmaz;
    /// aksi hâlde raster geometrisi örneğin 599 DPI ile hesaplanırken QPDL
    /// başlığı 300 DPI diyebilir.
    pub fn from_dpi_exact(dpi: u32) -> Option<Self> {
        match dpi {
            300 => Some(SplResolution::Dpi300),
            600 => Some(SplResolution::Dpi600),
            1200 => Some(SplResolution::Dpi1200),
            _ => None,
        }
    }

    /// PPD'nin ve ML-2160 ailesi için doğrulanmış QPDL modlarının sunduğu
    /// çözünürlük çiftlerini denetler. Eksenleri ayrı ayrı doğrulamak yeterli
    /// değildir: örneğin 600x1200 iki tanınan eksenden oluşsa da sunulan bir
    /// yazıcı modu değildir.
    pub fn pair_is_supported(x_dpi: u32, y_dpi: u32) -> bool {
        matches!(
            (x_dpi, y_dpi),
            (300, 300) | (600, 600) | (1200, 600) | (1200, 1200)
        )
    }
}

/// Duplex Modu
///
/// SpliX `request.cpp` PPD'deki `Duplex` seçeneğini, PPD'nin
/// `*QPDL ManualDuplex` özniteliğine bakarak OTOMATİK ya da ELLE duplex'e
/// eşler:
///
/// ```c
/// manualDuplex = ppd->get("ManualDuplex", "QPDL").isTrue();
/// if (value == "DuplexNoTumble") _duplex = manualDuplex ? ManualLongEdge : LongEdge;
/// else if (value == "DuplexTumble") _duplex = manualDuplex ? ManualShortEdge : ShortEdge;
/// else _duplex = Simplex;
/// ```
///
/// ML-2160 serisinin otomatik duplex donanımı YOKTUR: hem upstream SpliX'in
/// kardeş model PPD'leri (`ml1910.ppd`, `ml2010.ppd`, `ml2525.ppd`,
/// `ml1640.ppd`, `ml2510.ppd`) hem de bu projenin PPD'si
/// `*QPDL ManualDuplex: "On"` diyor. Dolayısıyla bu yazıcı ailesinde gerçekte
/// yalnızca `Simplex` ve `Manual*` varyantları tetiklenir; `LongEdge` ve
/// `ShortEdge` aynı QPDL v3 protokolünü konuşan otomatik duplekserli modeller
/// için modelde tutuluyor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SplDuplex {
    #[default]
    Simplex,
    LongEdge,
    ShortEdge,
    ManualLongEdge,
    ManualShortEdge,
}

/// Şerit Sıkıştırma Algoritması
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[repr(u8)]
pub enum SplCompression {
    #[default]
    None = 0x00,
    Rle = 0x11,
}

/// Yazıcının `@PJL SET PAPERTYPE` için tanıdığı kelime dağarcığı.
///
/// Kaynak, upstream SpliX'in aynı motor ailesine ait PPD'lerindeki
/// `*MediaType` seçenek anahtarlarıdır (ml1910.ppd, ml2010.ppd, ml2525.ppd,
/// ml1640.ppd, ml2510.ppd — beşi de birebir aynı listeyi veriyor). SpliX bu
/// anahtarı PPD'den okuyup `@PJL SET PAPERTYPE=%s` satırına olduğu gibi
/// yazar (printer.cpp sendPJLHeader), yani anahtarın kendisi protokol
/// değeridir; okunabilir bir etiket değildir.
///
/// `OFF` = "yazıcının kendi varsayılanını kullan" ve listenin ilk üyesidir;
/// `PJL_PAPERTYPE_DEFAULT` buna işaret eder.
pub const PJL_PAPER_TYPES: [&str; 14] = [
    "OFF", "NORMAL", "THICK", "THIN", "BOND", "OHP", "CARD", "LABEL", "USED", "COLOR", "ENV",
    "COTTON", "RECYCLED", "ARCHIVE",
];

/// Kağıt türü bilinmediğinde ya da tanınmadığında kullanılan PJL değeri.
pub const PJL_PAPERTYPE_DEFAULT: &str = PJL_PAPER_TYPES[0];

/// Serbest metin bir kağıt türü adını, yazıcının tanıdığı PJL değerine eşler.
///
/// Değer CUPS Raster başlığındaki `MediaType` alanından, yani işi gönderen
/// istemciden gelir ve GÜVENİLMEZDİR; bu yüzden metin doğrudan PJL satırına
/// yazılmaz, yalnızca tabloya eşlenir. Dönüş tipi `&'static str` olduğu için
/// PJL satırına yalnızca yukarıdaki sabitlerden biri girebilir: keyfi bir
/// dizenin (boşluk, CR/LF, ESC) satıra sızması tip düzeyinde imkânsızdır.
/// `PAPERTYPE` değeri `JOBNAME`/`USERNAME` gibi tırnak içinde taşınmadığı
/// için bu ayrım önemli — orada `sanitize_pjl_field` yeterliyken burada tek
/// bir boşluk bile satırı bozardı.
///
/// Karşılaştırma ASCII'de büyük/küçük harfe duyarsızdır: PPD anahtarları
/// büyük harfli, ama elle düzenlenmiş bir PPD'nin `env` yazması sessiz bir
/// geri düşüşe yol açmasın.
pub fn pjl_paper_type(name: &str) -> Option<&'static str> {
    let name = name.trim();
    PJL_PAPER_TYPES
        .iter()
        .find(|candidate| candidate.eq_ignore_ascii_case(name))
        .copied()
}

/// Unix çağından itibaren geçen tam gün sayısını Gregoryen takvim tarihine
/// çevirir. Algoritma yalnızca tam sayı aritmetiği kullanır; böylece filtre,
/// güncel servis tarihini üretmek için bir saat/tarih bağımlılığı taşımaz.
fn service_date_from_unix_days(days_since_epoch: i64) -> String {
    let z = days_since_epoch + 719_468;
    let era = (if z >= 0 { z } else { z - 146_096 }) / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += if month <= 2 { 1 } else { 0 };

    format!("{year:04}{month:02}{day:02}")
}

/// Geçerli UTC tarihini Samsung PJL'nin `YYYYMMDD` servis tarihi biçiminde
/// döndürür. Sistem saati Unix çağından önceyse güvenli ve geçerli bir taban
/// tarihi kullanılır.
pub fn current_service_date() -> String {
    let days_since_epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| (duration.as_secs() / 86_400) as i64)
        .unwrap_or(0);
    service_date_from_unix_days(days_since_epoch)
}

/// İş Konfigürasyonu
#[derive(Debug, Clone)]
pub struct JobConfig {
    pub job_name: String,
    pub user_name: String,
    pub service_date: String,
    pub duplex: SplDuplex,
    /// `@PJL SET PAPERTYPE` değeri.
    ///
    /// `String` değil `&'static str`: bkz. `pjl_paper_type`. Alan yalnızca
    /// `PJL_PAPER_TYPES` tablosundaki sabitlerden birini taşıyabildiği için,
    /// güvenilmez bir kağıt türü adının PJL satırına sızması mümkün değil.
    pub paper_type: &'static str,
}

impl Default for JobConfig {
    fn default() -> Self {
        Self {
            job_name: "CUPS Document".to_string(),
            user_name: "guest".to_string(),
            service_date: current_service_date(),
            duplex: SplDuplex::Simplex,
            paper_type: PJL_PAPERTYPE_DEFAULT,
        }
    }
}

/// Sayfa Konfigürasyonu
#[derive(Debug, Clone)]
pub struct PageConfig {
    pub paper_size: SplPaperSize,
    pub paper_source: SplPaperSource,
    /// Yatay (X) çözünürlük — QPDL sayfa başlığında `header[0x10]`.
    ///
    /// SpliX qpdl.cpp renderPage:
    ///   header[0x1]  = page->yResolution() / 100;
    ///   header[0x10] = page->xResolution() / 100;
    ///
    /// İki eksen AYRI alanlardır ve sıraları sezgiye aykırıdır (önce Y, sonra
    /// X). Daha önce tek bir `resolution` alanı vardı ve her iki bayta da
    /// YATAY çözünürlük yazılıyordu; simetrik modlarda (300/600/1200) bu fark
    /// edilmiyordu, ama `1200x600dpi` gibi gerçek bir QPDL modunda yazıcıya
    /// dikey çözünürlük 600 yerine 1200 olarak bildiriliyor ve sayfa dikeyde
    /// 2 kat eziliyordu.
    pub resolution_x: SplResolution,
    /// Dikey (Y) çözünürlük — QPDL sayfa başlığında `header[0x1]`.
    pub resolution_y: SplResolution,
    pub duplex: SplDuplex,
    /// Bu sayfanın iş içindeki 1 TABANLI sırası.
    ///
    /// Yalnızca `tumble` baytını (`header[0xC]`) üretmek için gerekli: SpliX
    /// bunu `page->pageNr() % 2` ile hesaplar ve `pageNr` 1'den başlar
    /// (document.cpp `_currentPage = 1`). 1 tabanlı olması sözleşmenin
    /// parçasıdır — 0 tabanlı bir sayaç pariteyi ters çevirir ve elle duplex
    /// baskıda sayfaların yanlış yüze gelmesine yol açar.
    pub page_number: u32,
    pub copies: u16,
    /// QPDL sayfa başlığındaki genişlik alanı 16-bit'tir; tip de öyle.
    ///
    /// Bu alanlar önce `u32`'ydi ve `begin_page` onları `>> 8` / `& 0xFF` ile
    /// sessizce kırpıyordu: 16-bit'e sığmayan bir değer, yazıcıya bildirilen
    /// boyut ile gerçek payload'ın uyuşmamasına (DMA/RLE çözme senkron kaybı)
    /// yol açardı. Genişlik için açık bir kontrol vardı, yükseklik için yoktu
    /// ve yükseklik yalnızca `validate_page_header`'daki `MAX_LINES` sınırı
    /// sayesinde DOLAYLI olarak korunuyordu. Alanları `u16` yapmak, çağıranı
    /// dönüşümü (`try_into`) açıkça ele almaya zorlar ve kırpılmayı tip
    /// düzeyinde imkânsız kılar.
    pub width_pixels: u16,
    pub height_pixels: u16,
    pub qpdl_version: u8,
}

impl Default for PageConfig {
    fn default() -> Self {
        Self {
            paper_size: SplPaperSize::A4,
            paper_source: SplPaperSource::Auto,
            resolution_x: SplResolution::Dpi600,
            resolution_y: SplResolution::Dpi600,
            duplex: SplDuplex::Simplex,
            page_number: 1,
            copies: 1,
            width_pixels: 4758,
            height_pixels: 6817,
            qpdl_version: 3,
        }
    }
}

// ============================================================================
// Algo 0x11 RLE Sıkıştırıcı (SpliX / Samsung QPDL Uyumlu)
// ============================================================================

pub struct Algo0x11;

impl Algo0x11 {
    pub fn lookup_best_offsets(data: &[u8]) -> [u16; TABLE_PTR_SIZE] {
        // Bu tablo 2048 x (u32, usize) = 32 KB tutar. Daha önce yığıtta
        // (stack) bir dizi olarak duruyordu: ana iş parçacığının 8 MB'lık
        // yığıtında sorun değil, ama kod ileride bir iş parçacığına taşınırsa
        // (varsayılan 2 MB) sessiz bir yığıt taşması riski oluşturur. Bant
        // başına bir tahsis, sıkıştırmanın kendi maliyetinin yanında ölçülemez
        // düzeyde kaldığı için öbeğe (heap) taşındı.
        let mut occurrences: Vec<(u32, usize)> =
            (0..COMPRESS_SAMPLE_RATE).map(|i| (0u32, i)).collect();

        // Veri örnekleme aralığından uzunsa her 2048 baytta bir örnek alınır;
        // kısaysa aralık hiç dolmayacağı için her bayt taranır. İki durumun
        // tek farkı tarama başlangıcı ve adımıdır.
        let (start, step) = if data.len() >= COMPRESS_SAMPLE_RATE {
            (COMPRESS_SAMPLE_RATE, COMPRESS_SAMPLE_RATE)
        } else {
            (1, 1)
        };

        let mut i = start;
        while i < data.len() {
            let b = data[i];
            let max_j = COMPRESS_SAMPLE_RATE.min(i);
            for j in 1..max_j {
                if data[i - j] == b {
                    occurrences[j - 1].0 += 1;
                }
            }
            i += step;
        }

        occurrences.sort_unstable_by(|a, b| b.0.cmp(&a.0));

        // En sık rastlanan 64 ofset. Tabloda 1 (bir önceki bayt) mutlaka
        // bulunmalı; yoksa son sıra ona ayrılır.
        let mut table = [1u16; TABLE_PTR_SIZE];
        for (slot, &(_, offset)) in table.iter_mut().zip(occurrences.iter()) {
            *slot = (offset + 1) as u16;
        }
        if !table.contains(&1) {
            table[TABLE_PTR_SIZE - 1] = 1;
        }

        table
    }

    pub fn compress(data: &[u8]) -> Option<Vec<u8>> {
        if data.is_empty() {
            return None;
        }

        let ptr_array = Self::lookup_best_offsets(data);
        let max_output_size = data.len() + 256;
        let mut out = Vec::with_capacity(max_output_size);

        // 1. uncompressed_initial_size için 4 bayt yer aç
        out.extend_from_slice(&[0u8; 4]);

        // 2. 64 adet u16 ofset tablosu
        let mut max_offset: usize = 0;
        for &ptr in &ptr_array {
            out.extend_from_slice(&ptr.to_le_bytes());
            if (ptr as usize) > max_offset {
                max_offset = ptr as usize;
            }
        }

        // 3. İlk ham baytlar
        let mut uncomp_size = max_offset.min(MAX_UNCOMPRESSED_BYTES).min(data.len());
        if uncomp_size == 0 {
            uncomp_size = 1.min(data.len());
        }

        let uncomp_size_u32 = uncomp_size as u32;
        out[0..4].copy_from_slice(&uncomp_size_u32.to_le_bytes());

        out.extend_from_slice(&data[..uncomp_size]);

        let mut r = uncomp_size;
        let mut raw_data_counter: usize = 0;
        let mut raw_data_counter_ptr: usize = 0;

        while r < data.len() {
            let max_comp_size = (data.len() - r).min(MAX_COMPRESSED_BYTES);

            if max_comp_size >= 3 {
                let mut best_comp_counter: usize = 0;
                let mut best_ptr: usize = 0;

                for (i, &offset) in ptr_array.iter().enumerate() {
                    let off = offset as usize;
                    if off > r {
                        continue;
                    }
                    let r_ref = r - off;

                    // Bu ofset mevcut en iyi eşleşmeyi GEÇEMEYECEKSE hiç
                    // karşılaştırma yapma: eşleşme uzunluğu ancak
                    // `best_comp_counter`'dan BÜYÜKSE dikkate alınıyor, o
                    // hâlde `best_comp_counter`ıncı bayt tutmuyorsa bu aday
                    // en fazla o kadar uzayabilir ve sonucu değiştiremez.
                    // Klasik LZ kısayolu; seçilen eşleşmeyi ve işaretçiyi
                    // aynen korur, yalnızca en kötü durumdaki bayt
                    // karşılaştırma sayısını düşürür.
                    //
                    // İndis güvenliği: döngü içindeyken `best_comp_counter`
                    // her zaman `max_comp_size`'dan KÜÇÜKTÜR (eşit olduğu anda
                    // aşağıda `break` edilir) ve `max_comp_size <= data.len() - r`
                    // olduğu için `r + best_comp_counter < data.len()`;
                    // `r_ref < r` olduğundan ikinci indis de sınır içindedir.
                    if data[r + best_comp_counter] != data[r_ref + best_comp_counter] {
                        continue;
                    }

                    let mut counter = 0;
                    while counter < max_comp_size && data[r + counter] == data[r_ref + counter] {
                        counter += 1;
                    }
                    if counter > best_comp_counter {
                        best_comp_counter = counter;
                        best_ptr = i;
                        if counter == max_comp_size {
                            break;
                        }
                    }
                }

                if best_comp_counter >= 3 {
                    if raw_data_counter > 0 {
                        out[raw_data_counter_ptr] = (raw_data_counter - 1) as u8;
                        raw_data_counter = 0;
                    }

                    r += best_comp_counter;
                    let comp_len = (best_comp_counter - 3) as u16;

                    let byte0 = COMPRESSION_FLAG | ((comp_len & 0x7F) as u8);
                    let byte1 = (((comp_len >> 1) & 0xC0) as u8) | ((best_ptr & 0x3F) as u8);

                    out.push(byte0);
                    out.push(byte1);
                    continue;
                }
            }

            raw_data_counter += 1;
            if raw_data_counter == 1 {
                raw_data_counter_ptr = out.len();
                out.push(0);
            } else if raw_data_counter == MAX_UNCOMPRESSED_BYTES {
                out[raw_data_counter_ptr] = 0x7F;
                raw_data_counter = 0;
            }

            out.push(data[r]);
            r += 1;
        }

        if raw_data_counter > 0 {
            out[raw_data_counter_ptr] = (raw_data_counter - 1) as u8;
        }

        Some(out)
    }

    pub fn calculate_checksum(data: &[u8]) -> u32 {
        let mut sum: u32 = 0;
        for &b in data {
            sum = sum.wrapping_add(b as u32);
        }
        sum
    }

    /// `compress`'in ürettiği akışı geri çözer (test/teşhis amaçlı).
    /// Format spec'inin (bkz. gerçek SpliX algo0x11.cpp) ters uygulamasıdır.
    ///
    /// Exposed under `golden-replay` as well as `test` for the reason Q-6
    /// gives: a `#[cfg(test)]` item is invisible to another crate's tests, and
    /// the filter's band-placement tests decompress the bands they assert on.
    #[cfg(any(test, feature = "golden-replay"))]
    pub fn decompress(data: &[u8]) -> Vec<u8> {
        let uncomp_size = u32::from_le_bytes(data[0..4].try_into().unwrap()) as usize;
        let mut ptr_array = [0u16; TABLE_PTR_SIZE];
        for (i, ptr) in ptr_array.iter_mut().enumerate() {
            let off = 4 + i * 2;
            *ptr = u16::from_le_bytes(data[off..off + 2].try_into().unwrap());
        }
        let table_end = 4 + TABLE_PTR_SIZE * 2;
        let mut out = Vec::new();
        out.extend_from_slice(&data[table_end..table_end + uncomp_size]);

        let mut pos = table_end + uncomp_size;
        while pos < data.len() {
            let b0 = data[pos];
            if b0 & COMPRESSION_FLAG != 0 {
                let b1 = data[pos + 1];
                let comp_len = ((b0 & 0x7F) as u16) | ((((b1 >> 6) & 0x3) as u16) << 7);
                let ptr_idx = (b1 & 0x3F) as usize;
                let offset = ptr_array[ptr_idx] as usize;
                let match_len = comp_len as usize + 3;
                let mut ref_pos = out.len() - offset;
                for _ in 0..match_len {
                    let b = out[ref_pos];
                    out.push(b);
                    ref_pos += 1;
                }
                pos += 2;
            } else {
                let count = (b0 as usize) + 1;
                pos += 1;
                out.extend_from_slice(&data[pos..pos + count]);
                pos += count;
            }
        }
        out
    }
}

// ============================================================================
// SPL2 / QPDL Akış Yöneticisi (SplStreamWriter)
// ============================================================================

/// PJL alanlarına (JOBNAME, USERNAME vb.) izin verilen azami BAYT sayısı.
///
/// PJL yorumlayıcıları genelde küçük, sabit boyutlu satır tamponları kullanır;
/// bu üst sınır, gerçekçi bir belge başlığını/kullanıcı adını kesmeden,
/// megabaytlarca metnin yazıcı firmware'ine gönderilmesini (olası çökme veya
/// tampon taşması) engeller.
///
/// Sınır kasıtlı olarak BAYT cinsindendir. Daha önce karakter sayılıyordu ve
/// `.take(128)` bir `char` iteratörü üzerinde çalıştığı için çok baytlı UTF-8
/// girdiyle gerçek sınır dört katına çıkabiliyordu: 400 emojilik bir iş adı
/// 531 baytlık bir PJL satırı üretiyordu. `sanitize_pjl_field` çıktısı artık
/// saf ASCII olduğu için bayt ve karakter sayısı da eşitlenmiş oluyor.
const MAX_PJL_FIELD_BYTES: usize = 128;

/// Bir karakterin, PJL alanına doğrudan yazılabilecek kadar güvenli olup
/// olmadığını söyler: yazdırılabilir ASCII (0x20-0x7E), çift tırnak hariç.
///
/// Çift tırnak, alıntılanmış (`"..."`) alanı erken kapatabilir; PJL'de bir
/// kaçış (escape) mekanizması olmadığı için kaçırmak yerine tamamen atılır.
#[inline]
fn is_safe_pjl_ascii(c: char) -> bool {
    matches!(c, ' '..='~') && c != '"'
}

/// ASCII dışı bir harfi, anlamını koruyan ASCII karşılığına katlar.
///
/// Beyaz liste saf ASCII olduğu için Türkçe bir iş adı ("Öğrenci Başvurusu")
/// aksi hâlde okunamaz hâle gelirdi ("renci Bavurusu"). Katlama, güvenlikten
/// ödün vermeden okunabilirliği koruyor: çıktı yine tamamen 0x20-0x7E
/// aralığında kalıyor ve yazıcı firmware'inin PJL sembol seti hakkında
/// hiçbir varsayım yapılmıyor.
///
/// Tablo Türkçe'yi tam kapsar; yaygın Batı Avrupa harfleri de eklenmiştir.
/// Listede olmayan ASCII dışı her karakter (CJK, emoji, semboller) atılır.
fn ascii_fold(c: char) -> Option<&'static str> {
    Some(match c {
        // --- Türkçe ---
        'ç' => "c",
        'Ç' => "C",
        'ğ' => "g",
        'Ğ' => "G",
        'ı' => "i",
        'İ' => "I",
        'ö' => "o",
        'Ö' => "O",
        'ş' => "s",
        'Ş' => "S",
        'ü' => "u",
        'Ü' => "U",
        'â' => "a",
        'Â' => "A",
        'î' => "i",
        'Î' => "I",
        'û' => "u",
        'Û' => "U",
        // --- Yaygın Batı Avrupa harfleri ---
        'á' | 'à' | 'ä' | 'ã' | 'å' => "a",
        'Á' | 'À' | 'Ä' | 'Ã' | 'Å' => "A",
        'é' | 'è' | 'ê' | 'ë' => "e",
        'É' | 'È' | 'Ê' | 'Ë' => "E",
        'í' | 'ì' | 'ï' => "i",
        'Í' | 'Ì' | 'Ï' => "I",
        'ó' | 'ò' | 'ô' | 'õ' | 'ø' => "o",
        'Ó' | 'Ò' | 'Ô' | 'Õ' | 'Ø' => "O",
        'ú' | 'ù' => "u",
        'Ú' | 'Ù' => "U",
        'ñ' => "n",
        'Ñ' => "N",
        'ý' | 'ÿ' => "y",
        'Ý' => "Y",
        'ð' => "d",
        'Ð' => "D",
        'þ' => "th",
        'Þ' => "TH",
        'æ' => "ae",
        'Æ' => "AE",
        'ß' => "ss",
        // --- Orta/Doğu Avrupa (Latin Extended-A/B) ---
        'ć' | 'č' | 'ĉ' | 'ċ' => "c",
        'Ć' | 'Č' | 'Ĉ' | 'Ċ' => "C",
        'ś' | 'š' | 'ș' | 'ŝ' => "s",
        'Ś' | 'Š' | 'Ș' | 'Ŝ' => "S",
        'ź' | 'ż' | 'ž' => "z",
        'Ź' | 'Ż' | 'Ž' => "Z",
        'ł' | 'ĺ' | 'ľ' => "l",
        'Ł' | 'Ĺ' | 'Ľ' => "L",
        'ń' | 'ň' | 'ņ' => "n",
        'Ń' | 'Ň' | 'Ņ' => "N",
        'đ' | 'ď' => "d",
        'Đ' | 'Ď' => "D",
        'ť' | 'ţ' | 'ț' => "t",
        'Ť' | 'Ţ' | 'Ț' => "T",
        'ř' | 'ŕ' => "r",
        'Ř' | 'Ŕ' => "R",
        'ě' | 'ē' | 'ė' | 'ę' | 'ĕ' => "e",
        'Ě' | 'Ē' | 'Ė' | 'Ę' | 'Ĕ' => "E",
        'ā' | 'ă' | 'ą' => "a",
        'Ā' | 'Ă' | 'Ą' => "A",
        'ī' | 'ĭ' | 'į' | 'ĩ' => "i",
        'Ī' | 'Ĭ' | 'Į' | 'Ĩ' => "I",
        'ō' | 'ŏ' | 'ő' => "o",
        'Ō' | 'Ŏ' | 'Ő' => "O",
        'ū' | 'ů' | 'ű' | 'ų' | 'ũ' | 'ŭ' => "u",
        'Ū' | 'Ů' | 'Ű' | 'Ų' | 'Ũ' => "U",
        'ġ' | 'ģ' => "g",
        'Ġ' | 'Ģ' => "G",
        'ķ' => "k",
        'Ķ' => "K",
        'ŷ' => "y",
        'Ŷ' | 'Ÿ' => "Y",
        'ŵ' => "w",
        'Ŵ' => "W",
        'ĵ' => "j",
        'Ĵ' => "J",
        'ħ' | 'ĥ' => "h",
        'Ħ' | 'Ĥ' => "H",
        'œ' => "oe",
        'Œ' => "OE",
        'ŀ' => "l",
        'Ŀ' => "L",
        'ŉ' => "n",
        'ŧ' => "t",
        'Ŧ' => "T",
        'ĳ' => "ij",
        'Ĳ' => "IJ",
        // --- Tipografik noktalama ---
        '\u{2018}' | '\u{2019}' => "'",
        // Eğri çift tırnaklar BOŞ dizeye katlanır, düz `"`'ye değil: düz tırnak
        // alıntılanmış PJL alanını erken kapatırdı (bkz. is_safe_pjl_ascii).
        '\u{201C}' | '\u{201D}' => "",
        '\u{2013}' | '\u{2014}' => "-",
        '\u{2026}' => "...",
        '\u{00A0}' => " ",
        _ => return None,
    })
}

/// PJL akışına gömülecek serbest metin alanlarını (JOBNAME, USERNAME vb.)
/// güvenli hale getirir.
///
/// PJL satır tabanlıdır ve alıntılanmış (`"..."`) dizeleri için bir kaçış
/// mekanizması yoktur: gömülen bir CR/LF ya da ESC (Universal Exit Language'ın
/// başlangıcı) baytı, alanı erken sonlandırıp ardından gelen metnin yazıcı
/// tarafından tamamen yeni, keyfi bir PJL komutu/işi olarak yorumlanmasına yol
/// açabilir. `job_name`/`user_name` CUPS iş başlığından (dolayısıyla işi
/// gönderen istemciden) geldiği için güvenilmez kabul edilmeli.
///
/// Süzgeç bir KARA LİSTE değil, bayt düzeyinde bir BEYAZ LİSTEdir. Önceki
/// `char::is_control()` tabanlı kara liste `char` düzeyinde çalışıyordu, oysa
/// yazıcıya giden şey bayt dizisidir: C1 aralığındaki (0x80-0x9F) her bayt,
/// çok baytlı bir UTF-8 karakterinin DEVAM BAYTI olarak süzgeçten geçebiliyordu.
/// Örneğin `U+02DB` telde `CB 9B` olur ve `0x9B` (C1 CSI) firmware'e ulaşırdı.
/// Beyaz liste bunu yapısal olarak imkânsız kılar: çıktının her baytı
/// 0x20-0x7E aralığındadır.
///
/// Enjeksiyon açısından kritik baytlar (`0x1B` ESC, `0x0A`, `0x0D`) zaten
/// UTF-8'de ne öncü ne devam baytı olarak görünemez; beyaz liste bunları da
/// kapsar ve garantiyi UTF-8 kodlamasının özelliğine değil, süzgecin kendisine
/// dayandırır.
///
/// Uzunluk `MAX_PJL_FIELD_BYTES` ile bayt cinsinden sınırlanır ve sınır,
/// katlama sonrası genişleyebilen diziler ("ß" -> "ss") da hesaba katılarak
/// üretim sırasında uygulanır.
fn sanitize_pjl_field(input: &str) -> String {
    let mut out = String::with_capacity(MAX_PJL_FIELD_BYTES);

    for c in input.chars() {
        // `piece` her zaman saf ASCII'dir, dolayısıyla len() == karakter sayısı.
        let piece: &str = if is_safe_pjl_ascii(c) {
            // Tek ASCII karakteri kopyalamak için geçici bir tampon gerekmesin
            // diye burada doğrudan itiyoruz.
            if out.len() + 1 > MAX_PJL_FIELD_BYTES {
                break;
            }
            out.push(c);
            continue;
        } else if let Some(folded) = ascii_fold(c) {
            folded
        } else {
            // Beyaz listede olmayan her şey (kontrol karakterleri, bidi
            // override, sıfır genişlikli karakterler, emoji, CJK) atılır.
            continue;
        };

        if out.len() + piece.len() > MAX_PJL_FIELD_BYTES {
            break;
        }
        out.push_str(piece);
    }

    debug_assert!(out.is_ascii(), "sanitize_pjl_field çıktısı ASCII olmalı");
    out
}

pub struct SplStreamWriter<W: Write> {
    writer: W,
    /// Sayfa içindeki bant sırası. QPDL kaydında 8 BİTLİK bir alan (`0x1`),
    /// ama sayaç burada `u16` tutuluyor ki 256. bantta sessizce sarmak yerine
    /// `u8::try_from` ile net bir hata versin (bkz. `write_compressed_band`).
    current_band: u16,
    /// `begin_job` çağrıldı ama `end_job` henüz çağrılmadı mı?
    ///
    /// `Drop` uygulaması bunu okuyarak, yarıda kalan bir işin kapanış UEL'ini
    /// yazmayı garanti eder; bkz. `impl Drop for SplStreamWriter`.
    job_active: bool,
}

impl<W: Write> SplStreamWriter<W> {
    pub fn new(writer: W) -> Self {
        Self {
            writer,
            current_band: 0,
            job_active: false,
        }
    }

    /// Samsung ML-2160 serisi PJL Başlığını gönderir.
    /// Borrows the sink this writer wraps.
    ///
    /// The CUPS filter writes straight to stdout, but PAPPL hands its driver a
    /// fresh device handle on every callback, so the printer application wraps
    /// a `Vec<u8>` and drains it here after each call. Nothing else needs it.
    pub fn writer_mut(&mut self) -> &mut W {
        &mut self.writer
    }

    pub fn begin_job(&mut self, config: &JobConfig) -> io::Result<()> {
        // Gerçek SpliX (printer.cpp sendPJLHeader) sırası: UEL'den sonra doğrudan
        // "@PJL DEFAULT SERVICEDATE=..." ile başlar; ayrı bir çıplak "@PJL\n" satırı
        // GÖNDERİLMEZ. PowerSave/JamRecovery satırları da bu projenin PPD
        // varsayılanlarına göre (PowerSave=5, JamRecovery=False) her zaman
        // gönderilir.
        let mut pjl = Vec::with_capacity(256);
        pjl.extend_from_slice(PJL_UEL);
        pjl.extend_from_slice(
            format!(
                "@PJL DEFAULT SERVICEDATE={}\n",
                sanitize_pjl_field(&config.service_date)
            )
            .as_bytes(),
        );
        pjl.extend_from_slice(
            format!(
                "@PJL SET USERNAME=\"{}\"\n",
                sanitize_pjl_field(&config.user_name)
            )
            .as_bytes(),
        );
        pjl.extend_from_slice(
            format!(
                "@PJL SET JOBNAME=\"{}\"\n",
                sanitize_pjl_field(&config.job_name)
            )
            .as_bytes(),
        );
        pjl.extend_from_slice(b"@PJL DEFAULT POWERSAVE=ON\n");
        pjl.extend_from_slice(b"@PJL DEFAULT POWERSAVETIME=5\n");
        pjl.extend_from_slice(b"@PJL SET JAMRECOVERY=OFF\n");

        // Gerçek SpliX (printer.cpp sendPJLHeader) duplex durumuna göre
        // DUPLEX=ON/OFF ve (açıksa) BINDING=LONGEDGE/SHORTEDGE gönderir.
        // SpliX printer.cpp sendPJLHeader; ELLE duplex `ON` değil `MANUAL`
        // gönderir ve bu ayrım ML-2160 ailesi için önemlidir, çünkü bu
        // modellerde duplex her zaman elledir (bkz. SplDuplex).
        match config.duplex {
            SplDuplex::Simplex => {
                pjl.extend_from_slice(b"@PJL SET DUPLEX=OFF\n");
            }
            SplDuplex::LongEdge => {
                pjl.extend_from_slice(b"@PJL SET DUPLEX=ON\n");
                pjl.extend_from_slice(b"@PJL SET BINDING=LONGEDGE\n");
            }
            SplDuplex::ShortEdge => {
                pjl.extend_from_slice(b"@PJL SET DUPLEX=ON\n");
                pjl.extend_from_slice(b"@PJL SET BINDING=SHORTEDGE\n");
            }
            SplDuplex::ManualLongEdge => {
                pjl.extend_from_slice(b"@PJL SET DUPLEX=MANUAL\n");
                pjl.extend_from_slice(b"@PJL SET BINDING=LONGEDGE\n");
            }
            SplDuplex::ManualShortEdge => {
                pjl.extend_from_slice(b"@PJL SET DUPLEX=MANUAL\n");
                pjl.extend_from_slice(b"@PJL SET BINDING=SHORTEDGE\n");
            }
        }

        // SpliX printer.cpp sendPJLHeader, PPD'de bir `*MediaType` seçeneği
        // varsa onun anahtarını `@PJL SET PAPERTYPE=%s` olarak gönderir,
        // yoksa `OFF` yazar. Bu filtre seçeneği PPD'den değil CUPS Raster
        // sayfa başlığındaki `MediaType` alanından okur (bkz. main.rs
        // `pjl_paper_type_for`); `config.paper_type` oraya kadar `&'static
        // str` olarak geldiği için burada ek bir süzgeç gerekmiyor.
        pjl.extend_from_slice(format!("@PJL SET PAPERTYPE={}\n", config.paper_type).as_bytes());
        pjl.extend_from_slice(b"@PJL SET ALTITUDE=LOW\n");
        pjl.extend_from_slice(b"@PJL SET DENSITY=3\n");
        pjl.extend_from_slice(b"@PJL SET RET=NORMAL\n");
        pjl.extend_from_slice(b"@PJL ENTER LANGUAGE = QPDL\n");

        self.writer.write_all(&pjl)?;
        self.writer.flush()?;

        // Bu noktadan itibaren yazıcı QPDL dilindedir ve akışın mutlaka bir
        // kapanış UEL'i ile bitmesi gerekir.
        self.job_active = true;
        Ok(())
    }

    /// Samsung QPDL 17-Baytlık Sayfa Başlığını Gönderir.
    pub fn begin_page(&mut self, config: &PageConfig) -> io::Result<()> {
        self.current_band = 0;

        let mut header = [0u8; 17];
        header[0x0] = 0x00; // Sayfa Başlığı İmzası
        header[0x1] = (config.resolution_y.dpi() / 100) as u8; // DİKEY (Y) çözünürlük / 100
        header[0x2] = (config.copies >> 8) as u8;
        header[0x3] = (config.copies & 0xFF) as u8;
        header[0x4] = config.paper_size as u8; // A4 = 2
        header[0x5..0x7].copy_from_slice(&config.width_pixels.to_be_bytes());
        header[0x7..0x9].copy_from_slice(&config.height_pixels.to_be_bytes());
        header[0x9] = config.paper_source as u8; // Auto = 1
        header[0xA] = 0x00; // unknownByte1

        // SpliX qpdl.cpp renderPage, duplex/tumble baytları:
        //
        //   Simplex         : duplex = 1, tumble = 0
        //   LongEdge        : duplex = 1, tumble = pageNr % 2
        //   ShortEdge       : duplex = 0, tumble = pageNr % 2
        //   ManualLongEdge  : duplex = 0, tumble = pageNr % 2
        //   ManualShortEdge : duplex = 0, tumble = pageNr % 2
        //
        // `duplex` baytı sezgiye aykırıdır: Simplex'te 1, çift taraflı elle
        // baskıda 0'dır. Bu değer eskiden doğru üretiliyordu, ama `tumble`
        // koşulsuz 0 yazılıyordu; oysa SpliX'te SAYFA NUMARASININ PARİTESİDİR
        // ve `_currentPage` 1'den başlar (document.cpp: `_currentPage = 1`),
        // yani tek numaralı sayfalarda 1, çift numaralılarda 0 olur.
        header[0xB] = match config.duplex {
            SplDuplex::Simplex | SplDuplex::LongEdge => 1,
            SplDuplex::ShortEdge | SplDuplex::ManualLongEdge | SplDuplex::ManualShortEdge => 0,
        };
        header[0xC] = match config.duplex {
            SplDuplex::Simplex => 0,
            _ => (config.page_number % 2) as u8,
        };
        header[0xD] = 0x00; // unknownByte2
        header[0xE] = config.qpdl_version; // 3
        header[0xF] = 0x01; // Colorplanes = 1 (Monokrom)
        header[0x10] = (config.resolution_x.dpi() / 100) as u8; // YATAY (X) çözünürlük / 100

        self.writer.write_all(&header)?;
        Ok(())
    }

    /// Raster şeridini Algo 0x11 RLE sıkıştırması, alt-başlık ve checksum ile yazar.
    ///
    /// `band_width_pixels`: Yazıcı DMA motorunun satır atlama genişliği.
    /// Satır kaymasını (zebra / merdiven deseni) önlemek için mutlaka tam bayt hizalı `bytes_per_line * 8` olmalıdır!
    pub fn write_compressed_band(
        &mut self,
        band_width_pixels: u16,
        band_height_lines: u16,
        raw_bitmap: &[u8],
    ) -> io::Result<()> {
        // Gerçek SpliX (compress.cpp compressPage/_compressBandedPage) hiçbir zaman
        // ham/sıkıştırmasız (0x00) bant göndermez: yalnızca 0x0D/0x0E/0x11/0x13/0x15
        // algoritmalarından biri kullanılır. Bu yazıcı ailesinde ham bant firmware
        // tarafından tanınmıyor ("INTERNAL ERROR - Please use the proper driver"
        // ile reddediliyor); bu yüzden burada da her zaman Algo 0x11 RLE kullanılır.
        // `unwrap_or_default()` DEĞİL: `compress` bir `None` döndürdüğünde
        // (bugün yalnızca girdi boşken; `band_size = bw_bytes * band_height`
        // olduğu ve ikisi de sıfırdan büyük olduğu için pratikte erişilemez)
        // boş bir `Vec` kullanmak, başlığı "Algo 0x11 ile sıkıştırılmış" diyen
        // ama payload'u 0 bayt olan BOZUK bir bant kaydı üretirdi — yazıcı
        // tarafında sessiz bir çözme hatası. Hata hatadır: yukarı bildiriliyor
        // ve akış `Drop` üzerinden kapanış UEL'i ile düzgünce sonlandırılıyor.
        let payload_bytes = Algo0x11::compress(raw_bitmap).ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Algo 0x11 sıkıştırıcısı bant için payload üretemedi (boş bant verisi).",
            )
        })?;
        let compression_type = SplCompression::Rle;

        // Toplam veri boyutu: payload + 4 (sub-header sig) + 4 (checksum)
        let total_data_size = (payload_bytes.len() + 8) as u32;

        // QPDL bant sırası alanı 8 bitliktir. Bugün taşması imkânsız: kabul
        // edilen en büyük sayfada (Legal @1200 DPI => 16800 satır) 128
        // satırlık bantlarla 132 bant eder. Ama kâğıt tablosu ya da
        // `QPDL_BAND_HEIGHT` değiştiğinde sessizce bozulabilecek ÖRTÜK bir
        // invaryanttı: `wrapping_add` 256. bantta sırayı 0'a döndürüp yazıcıya
        // aynı sıra numarasını ikinci kez bildirirdi. Artık invaryant örtük
        // değil, uygulanıyor.
        let band_index = u8::try_from(self.current_band).map_err(|_| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "Sayfa {} banttan fazlasını içeriyor: QPDL bant sırası alanı 8 bitlik, \
                     bu yüzden bir sayfada en çok {} bant taşınabilir.",
                    u8::MAX as u16 + 1,
                    u8::MAX as u16 + 1
                ),
            )
        })?;

        // 1. Şerit Başlığı (Record 0x0C - 11 Bayt)
        let mut band_header = [0u8; 11];
        band_header[0x0] = 0x0C; // Signature 12
        band_header[0x1] = band_index;
        band_header[0x2] = (band_width_pixels >> 8) as u8;
        band_header[0x3] = (band_width_pixels & 0xFF) as u8;
        band_header[0x4] = (band_height_lines >> 8) as u8;
        band_header[0x5] = (band_height_lines & 0xFF) as u8;
        band_header[0x6] = compression_type as u8; // 0x11 (Algo 0x11 RLE)
        band_header[0x7] = (total_data_size >> 24) as u8;
        band_header[0x8] = (total_data_size >> 16) as u8;
        band_header[0x9] = (total_data_size >> 8) as u8;
        band_header[0xA] = (total_data_size & 0xFF) as u8;

        self.writer.write_all(&band_header)?;

        // 2. Alt Başlık İmzası (Sub-header: 0x09ABCDEF LE)
        self.writer.write_all(&SUBHEADER_SIG_LE)?;

        // 3. Sıkıştırılmış (Algo 0x11 RLE) Şerit Verisi
        self.writer.write_all(&payload_bytes)?;

        // 4. Sağlama Toplamı (Checksum: Subheader + Payload toplamı)
        let mut checksum = Algo0x11::calculate_checksum(&SUBHEADER_SIG_LE);
        checksum = checksum.wrapping_add(Algo0x11::calculate_checksum(&payload_bytes));

        let checksum_bytes = checksum.to_be_bytes();
        self.writer.write_all(&checksum_bytes)?;

        self.current_band += 1;
        Ok(())
    }

    /// Sayfa Sonu (3 baytlık QPDL Page Footer: [0x01, copies_msb, copies_lsb])
    pub fn end_page(&mut self, copies: u16) -> io::Result<()> {
        let footer = [
            0x01, // Page Footer İmzası
            (copies >> 8) as u8,
            (copies & 0xFF) as u8,
        ];
        self.writer.write_all(&footer)?;
        self.writer.flush()?;
        Ok(())
    }

    /// İş Sonu (PJL UEL Kapanışı)
    ///
    /// Gerçek SpliX (printer.cpp sendPJLFooter) `_endPJL`'i olduğu gibi yazıp
    /// hemen flush eder; sona fazladan bir "\n" EKLEMEZ.
    ///
    /// Çağrı idempotenttir: `begin_job` çağrılmamışsa ya da iş zaten
    /// kapatılmışsa hiçbir şey yazmaz. Böylece `Drop` içindeki güvenlik ağı
    /// (aşağıya bkz.) başarılı akışlarda ikinci bir UEL üretmez.
    pub fn end_job(&mut self) -> io::Result<()> {
        if !self.job_active {
            return Ok(());
        }
        // Bayrağı yazmadan ÖNCE düşür: yazma başarısızsa (ör. backend boruyu
        // kapattıysa) `Drop` aynı başarısız yazmayı tekrar denemesin.
        self.job_active = false;
        self.writer.write_all(PJL_END)?;
        self.writer.flush()?;
        Ok(())
    }
}

/// Yarıda kalan bir işi kapanış UEL'i ile sonlandıran güvenlik ağı.
///
/// `begin_job` `@PJL ENTER LANGUAGE = QPDL` yazdığı anda yazıcı QPDL diline
/// geçer. Akış kapanış UEL'i olmadan biterse yazıcı bu dilde, tamamlanmamış
/// bir bant kaydını bekler hâlde asılı kalır ve sıradaki iş de bu artık
/// duruma girer. Önceden `end_job` yalnızca sayfa döngüsü BAŞARIYLA bittiğinde
/// çağrılıyordu; aradaki her `?` (başlık doğrulama hatası, kısa okuma, bozuk
/// sayfa) akışı yarım bırakıyordu.
///
/// DİKKAT: `std::process::exit` yığındaki `Drop`'ları ÇALIŞTIRMAZ. Bu ağın
/// işlemesi için writer'ın, hata `main`'e ulaşıp `exit` çağrılmadan önce
/// düşürülmüş olması gerekir — bu yüzden writer `process_cups_raster_to_spl`
/// içinde yerel olarak yaşar ve hata `?` ile döndürülür.
impl<W: Write> Drop for SplStreamWriter<W> {
    fn drop(&mut self) {
        if self.job_active {
            // Hatayı raporlayacak bir yer yok; akışı kapatmaya çalışmak yine de
            // hiç denememekten iyidir.
            let _ = self.end_job();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// QPDL bant sırası alanı 8 bitliktir; 257. bant sessizce 0'a sarmak
    /// yerine hata vermeli. Bugün erişilemez (en kötü durum 170 bant), ama
    /// invaryant `MAX_POINTS`/`QPDL_BAND_HEIGHT` değiştiğinde bozulmasın diye
    /// pinleniyor.
    #[test]
    fn test_band_index_beyond_255_is_rejected_not_wrapped() {
        let mut out: Vec<u8> = Vec::new();
        let mut w = SplStreamWriter::new(&mut out);
        let band = vec![0u8; 8];
        for i in 0..=255u16 {
            w.write_compressed_band(64, 1, &band)
                .unwrap_or_else(|e| panic!("bant {} reddedildi: {}", i, e));
        }
        let err = w
            .write_compressed_band(64, 1, &band)
            .expect_err("257. bant hata vermeli");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("8 bitlik"), "{}", err);
    }

    /// Sayaç sayfa başına sıfırlanır: art arda gelen sayfalar birbirinin
    /// bant sırasını tüketmemeli.
    #[test]
    fn test_band_index_resets_on_each_page() {
        let mut out: Vec<u8> = Vec::new();
        let mut w = SplStreamWriter::new(&mut out);
        let band = vec![0u8; 8];
        for _ in 0..2 {
            w.begin_page(&PageConfig::default()).unwrap();
            for _ in 0..200 {
                w.write_compressed_band(64, 1, &band).unwrap();
            }
        }
    }

    /// Tanınmayan bir ölçü A4'e ya da başka bir QPDL koduna düşürülmemeli.
    #[test]
    fn test_unknown_paper_size_is_rejected_instead_of_falling_back_to_a4() {
        assert_eq!(
            SplPaperSize::from_dimensions_pt_exact(595, 842),
            Some(SplPaperSize::A4)
        );
        assert_eq!(SplPaperSize::from_dimensions_pt_exact(612, 936), None);
    }

    /// DPI dönüşümü yaklaşık değerleri sessizce 300/600/1200'e yuvarlamamalı;
    /// yalnızca PPD'nin sunduğu eksen değerleri ve doğrulanmış çiftler geçerli.
    #[test]
    fn test_resolution_mapping_is_exact_and_pair_aware() {
        assert_eq!(
            SplResolution::from_dpi_exact(300),
            Some(SplResolution::Dpi300)
        );
        assert_eq!(
            SplResolution::from_dpi_exact(600),
            Some(SplResolution::Dpi600)
        );
        assert_eq!(
            SplResolution::from_dpi_exact(1200),
            Some(SplResolution::Dpi1200)
        );
        for unsupported in [0, 299, 599, 601, 1199, 1201] {
            assert_eq!(SplResolution::from_dpi_exact(unsupported), None);
        }

        assert!(SplResolution::pair_is_supported(300, 300));
        assert!(SplResolution::pair_is_supported(600, 600));
        assert!(SplResolution::pair_is_supported(1200, 600));
        assert!(SplResolution::pair_is_supported(1200, 1200));
        assert!(!SplResolution::pair_is_supported(600, 1200));
        assert!(!SplResolution::pair_is_supported(300, 600));
    }

    #[test]
    fn test_service_date_conversion_handles_epoch_and_leap_day() {
        assert_eq!(service_date_from_unix_days(-1), "19691231");
        assert_eq!(service_date_from_unix_days(0), "19700101");
        assert_eq!(service_date_from_unix_days(11_016), "20000229");
    }

    /// Sıkıştırıcı payload üretemezse bant kaydı BOŞ payload'la yazılmamalı.
    #[test]
    fn test_empty_band_data_is_an_error_not_an_empty_record() {
        let mut out: Vec<u8> = Vec::new();
        {
            let mut w = SplStreamWriter::new(&mut out);
            let err = w
                .write_compressed_band(64, 1, &[])
                .expect_err("boş bant verisi hata vermeli");
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        }
        // `begin_job` çağrılmadığı için `Drop` da bir şey yazmamalı.
        assert!(out.is_empty(), "hata durumunda hiçbir bayt yazılmamalı");
    }

    #[test]
    fn test_sanitize_pjl_field_strips_injection_chars() {
        assert_eq!(sanitize_pjl_field("Normal Title"), "Normal Title");
        // Çift tırnak: alıntılanmış alanı erken kapatabilir.
        assert_eq!(sanitize_pjl_field("a\"b"), "ab");
        // CR/LF: yeni bir PJL komut satırı başlatabilir.
        assert_eq!(sanitize_pjl_field("a\r\nb"), "ab");
        // ESC: Universal Exit Language (@PJL zarfını erken bitirir).
        assert_eq!(sanitize_pjl_field("a\x1bb"), "ab");
        // Tam enjeksiyon denemesi: yeni bir @PJL komutu eklemeye çalışıyor.
        let injected = sanitize_pjl_field("x\"\n@PJL DEFAULT PASSWORD=1234\n");
        assert!(!injected.contains('"'));
        assert!(!injected.contains('\n'));
        assert!(!injected.contains('\r'));
    }

    #[test]
    fn test_sanitize_pjl_field_strips_unicode_line_separators() {
        // U+2028/U+2029: is_control() bunları yakalamaz, ama bazı metin
        // işleyicileri satır sonu sayabilir.
        assert_eq!(sanitize_pjl_field("a\u{2028}b"), "ab");
        assert_eq!(sanitize_pjl_field("a\u{2029}b"), "ab");
    }

    #[test]
    fn test_sanitize_pjl_field_strips_bidi_override_spoofing() {
        // U+202E (RLO) ile "evil.exe" gibi bir dosya adı sahteciliği
        // tekniğinin PJL alanlarında kullanıcı/iş adını farklı göstermesi
        // engellenmeli.
        let spoofed = sanitize_pjl_field("Invoice\u{202E}cod.exe");
        assert!(!spoofed.contains('\u{202E}'));
        assert_eq!(spoofed, "Invoicecod.exe");

        // Diğer bidi gömme/izolasyon ve sıfır genişlikli karakterler de.
        for c in ['\u{202A}', '\u{2066}', '\u{200B}', '\u{FEFF}'] {
            let s = format!("a{}b", c);
            assert_eq!(sanitize_pjl_field(&s), "ab", "char {:?} süzülmedi", c);
        }
    }

    /// Beyaz liste saf ASCII olduğu için Türkçe metin katlanarak korunur;
    /// harfler sessizce düşürülmez ("renci Bavurusu" olmaz).
    #[test]
    fn test_sanitize_pjl_field_folds_turkish_to_ascii() {
        assert_eq!(sanitize_pjl_field("Öğrenci Başvurusu"), "Ogrenci Basvurusu");
        assert_eq!(sanitize_pjl_field("Çiğdem ÜNLÜ"), "Cigdem UNLU");
        assert_eq!(sanitize_pjl_field("ışık İSTANBUL"), "isik ISTANBUL");
    }

    /// Katlama tablosunda olmayan ASCII dışı karakterler atılır; çevrelerindeki
    /// meşru metin korunur.
    #[test]
    fn test_sanitize_pjl_field_drops_unmappable_non_ascii() {
        assert_eq!(sanitize_pjl_field("Rapor \u{1F600} 2026"), "Rapor  2026");
        assert_eq!(sanitize_pjl_field("会議 notes"), " notes");
    }

    /// Katlama tablosu, Latin-1 Supplement'teki TÜM harfleri kapsamalı.
    ///
    /// Beyaz liste güvenliği tablodan bağımsız garanti eder (kapsanmayan
    /// karakter atılır, sızmaz); tablonun kapsamı yalnızca OKUNABİLİRLİĞİ
    /// belirler. Bu test, Batı Avrupa alfabesindeki hiçbir harfin sessizce
    /// düşmemesini sabitler.
    #[test]
    fn test_ascii_fold_covers_all_latin1_letters() {
        let mut missing = Vec::new();
        for cp in 0x00C0u32..=0x00FF {
            let c = char::from_u32(cp).unwrap();
            if c == '\u{00D7}' || c == '\u{00F7}' {
                continue; // × ve ÷ harf değil, çarpma/bölme işaretleri
            }
            match ascii_fold(c) {
                Some(folded) if !folded.is_empty() && folded.is_ascii() => {}
                _ => missing.push(format!("U+{:04X} ({})", cp, c)),
            }
        }
        assert!(
            missing.is_empty(),
            "katlanamayan Latin-1 harfleri: {:?}",
            missing
        );
    }

    /// Katlama tablosunun HER çıktısı yazdırılabilir ASCII olmalı ve alanı
    /// bozacak bir karakter içermemeli.
    #[test]
    fn test_ascii_fold_outputs_are_always_safe() {
        for cp in 0u32..0x2500 {
            if let Some(c) = char::from_u32(cp) {
                if let Some(folded) = ascii_fold(c) {
                    assert!(
                        folded.bytes().all(|b| (0x20..=0x7E).contains(&b)),
                        "U+{:04X} yazdırılamayan bayta katlandı: {:?}",
                        cp,
                        folded
                    );
                    assert!(
                        !folded.contains('"'),
                        "U+{:04X} çift tırnağa katlandı; alıntılanmış alanı bozar",
                        cp
                    );
                }
            }
        }
    }

    /// O-03 regresyonu: çıktının HER baytı 0x20-0x7E aralığında olmalı.
    ///
    /// Eski `char::is_control()` kara listesi `char` düzeyinde çalıştığı için
    /// C1 aralığındaki baytlar çok baytlı UTF-8'in devam baytı olarak
    /// sızabiliyordu: `U+02DB` telde `CB 9B` olur ve `0x9B` (C1 CSI)
    /// firmware'e ulaşırdı.
    #[test]
    fn test_sanitize_pjl_field_output_is_always_printable_ascii() {
        let hostile = "A\u{02DB}B\u{0219}C\u{1E9B}D\u{FF02}E\u{202E}F\u{1F4A9}";
        let out = sanitize_pjl_field(hostile);
        assert!(
            out.bytes().all(|b| (0x20..=0x7E).contains(&b)),
            "yazdırılamayan bayt sızdı: {:?}",
            out.as_bytes()
        );
        assert!(!out.as_bytes().contains(&0x9B), "C1 CSI baytı sızdı");
        assert!(!out.contains('"'));

        // Aynı garanti geniş bir Unicode taramasında da geçerli olmalı.
        for cp in (0u32..0x3000).chain(0x1F300..0x1F400) {
            if let Some(c) = char::from_u32(cp) {
                let s = format!("x{}y", c);
                let out = sanitize_pjl_field(&s);
                assert!(
                    out.bytes().all(|b| (0x20..=0x7E).contains(&b)) && !out.contains('"'),
                    "U+{:04X} güvensiz bayt üretti: {:?}",
                    cp,
                    out.as_bytes()
                );
            }
        }
    }

    /// O-02 regresyonu: sınır BAYT cinsinden olmalı.
    ///
    /// Karakter sayan eski sınır, çok baytlı UTF-8 ile gerçek satır uzunluğunu
    /// dört katına çıkarabiliyordu (400 emojilik başlık -> 531 baytlık satır).
    #[test]
    fn test_sanitize_pjl_field_enforces_max_byte_length() {
        let huge = "A".repeat(1_000_000);
        assert_eq!(sanitize_pjl_field(&huge).len(), MAX_PJL_FIELD_BYTES);

        // Çok baytlı girdi de aynı BAYT sınırına uymalı.
        for probe in ["Ö", "\u{1F600}", "ß", "ğ"] {
            let s = probe.repeat(4000);
            let out = sanitize_pjl_field(&s);
            assert!(
                out.len() <= MAX_PJL_FIELD_BYTES,
                "{:?} sınırı aştı: {} bayt",
                probe,
                out.len()
            );
        }

        let short = "short title";
        assert_eq!(sanitize_pjl_field(short), short);
    }

    /// Katlanınca genişleyen diziler ("ß" -> "ss") sınırı taşırmamalı.
    #[test]
    fn test_sanitize_pjl_field_respects_limit_for_expanding_folds() {
        let out = sanitize_pjl_field(&"ß".repeat(200));
        assert!(out.len() <= MAX_PJL_FIELD_BYTES);
        assert!(out.bytes().all(|b| b == b's'));
    }

    /// Üretilen PJL satırının tamamı bayt sınırının makul bir katında kalmalı.
    #[test]
    fn test_pjl_lines_stay_short_with_multibyte_input() {
        let cfg = JobConfig {
            job_name: "\u{1F600}".repeat(400),
            user_name: "Ö".repeat(400),
            service_date: "20120101".to_string(),
            duplex: SplDuplex::Simplex,
            paper_type: PJL_PAPERTYPE_DEFAULT,
        };
        let mut out: Vec<u8> = Vec::new();
        {
            let mut w = SplStreamWriter::new(&mut out);
            w.begin_job(&cfg).unwrap();
            w.end_job().unwrap();
        }
        for line in out.split(|&b| b == b'\n') {
            assert!(
                line.len() <= MAX_PJL_FIELD_BYTES + 64,
                "aşırı uzun PJL satırı: {} bayt",
                line.len()
            );
        }
    }

    #[test]
    fn test_begin_job_blocks_pjl_line_injection() {
        // Kötü niyetli girdi: tırnak + CR/LF ile alıntılanmış alandan kaçıp
        // yeni bir "@PJL DEFAULT PASSWORD=..." satırı enjekte etmeye,
        // ayrıca ESC (UEL) ile PJL zarfını erken bitirip yeni bir zarf/iş
        // başlatmaya çalışıyor.
        let malicious = JobConfig {
            job_name: "Evil\"\n@PJL DEFAULT PASSWORD=1234".to_string(),
            user_name: "attacker\x1b%-12345X@PJL SET JOBNAME=\"hijacked".to_string(),
            service_date: "20120101".to_string(),
            duplex: SplDuplex::Simplex,
            paper_type: PJL_PAPERTYPE_DEFAULT,
        };
        let benign = JobConfig {
            job_name: "Benign".to_string(),
            user_name: "benign".to_string(),
            service_date: "20120101".to_string(),
            duplex: SplDuplex::Simplex,
            paper_type: PJL_PAPERTYPE_DEFAULT,
        };

        // İşler açıkça kapatılır: writer düşerken `Drop` kapanış UEL'ini
        // yazacağı için (bkz. test_drop_writes_closing_uel_for_unfinished_job)
        // iki akış da tam bir iş olsun, aşağıdaki UEL sayımı yalnızca
        // ENJEKTE EDİLMİŞ zarfları ölçsün.
        let mut out_malicious: Vec<u8> = Vec::new();
        {
            let mut w = SplStreamWriter::new(&mut out_malicious);
            w.begin_job(&malicious).unwrap();
            w.end_job().unwrap();
        }
        let mut out_benign: Vec<u8> = Vec::new();
        {
            let mut w = SplStreamWriter::new(&mut out_benign);
            w.begin_job(&benign).unwrap();
            w.end_job().unwrap();
        }

        // Kötü niyetli girdi, zararsız girdiyle AYNI sayıda PJL satırı
        // üretmeli: sanitizasyon başarısız olsaydı gömülü "\n" ekstra
        // satır(lar) enjekte ederdi.
        let lines_malicious = out_malicious.iter().filter(|&&b| b == b'\n').count();
        let lines_benign = out_benign.iter().filter(|&&b| b == b'\n').count();
        assert_eq!(
            lines_malicious, lines_benign,
            "girdi ekstra PJL komut satırı enjekte edebildi"
        );

        // Akışta yalnızca işin kendi açılış ve kapanış UEL'i bulunmalı;
        // girdi üçüncü bir zarf (yeni bir iş başlangıcı) ekleyememeli.
        let uel_count = count_uel(&out_malicious);
        assert_eq!(uel_count, count_uel(&out_benign));
        assert_eq!(uel_count, 2, "girdi yeni bir UEL zarfı enjekte edebildi");
    }

    /// Akıştaki UEL (yeni PJL zarfı) sayısını döner.
    fn count_uel(stream: &[u8]) -> usize {
        stream
            .windows(PJL_UEL.len())
            .filter(|w| *w == PJL_UEL)
            .count()
    }

    /// Y-03 regresyonu: `begin_job` çağrıldıktan sonra `end_job` çağrılmadan
    /// writer düşerse, `Drop` kapanış UEL'ini yazmalı.
    ///
    /// `begin_job` yazıcıyı `@PJL ENTER LANGUAGE = QPDL` ile QPDL diline
    /// sokar; akış UEL olmadan biterse yazıcı bu dilde asılı kalır ve sonraki
    /// iş de bozulur.
    #[test]
    fn test_drop_writes_closing_uel_for_unfinished_job() {
        let mut out: Vec<u8> = Vec::new();
        {
            let mut w = SplStreamWriter::new(&mut out);
            w.begin_job(&JobConfig::default()).unwrap();
            w.begin_page(&PageConfig::default()).unwrap();
            // end_job YOK: hata yolunu taklit ediyoruz.
        }
        assert!(
            out.ends_with(PJL_END),
            "yarım kalan iş kapanış UEL'i olmadan bitti"
        );
        assert_eq!(count_uel(&out), 2, "açılış + kapanış UEL'i beklenir");
    }

    /// `end_job` açıkça çağrıldıysa `Drop` ikinci bir UEL yazmamalı.
    #[test]
    fn test_drop_does_not_duplicate_uel_after_end_job() {
        let mut out: Vec<u8> = Vec::new();
        {
            let mut w = SplStreamWriter::new(&mut out);
            w.begin_job(&JobConfig::default()).unwrap();
            w.end_job().unwrap();
        }
        assert_eq!(count_uel(&out), 2, "Drop fazladan UEL yazdı");
    }

    /// `begin_job` hiç çağrılmadıysa `Drop` hiçbir şey yazmamalı: yazıcı QPDL
    /// diline hiç girmemiştir, yalnız başına bir UEL göndermenin anlamı yok.
    #[test]
    fn test_drop_writes_nothing_when_job_never_started() {
        let mut out: Vec<u8> = Vec::new();
        {
            let _w = SplStreamWriter::new(&mut out);
        }
        assert!(out.is_empty(), "iş başlamadan çıktı üretildi: {:?}", out);
    }

    /// D-02 regresyonu: QPDL sayfa başlığındaki 16-bit genişlik/yükseklik
    /// alanları büyük-endian olarak, kırpılmadan yazılmalı.
    ///
    /// Alanlar `u16` olduğu için `u32`'den sessiz bir daralma artık tip
    /// düzeyinde imkânsız; bu test kodlamanın kendisini sabitler.
    #[test]
    fn test_begin_page_encodes_16bit_dimensions() {
        let cfg = PageConfig {
            width_pixels: u16::MAX,
            height_pixels: 0xABCD,
            ..PageConfig::default()
        };
        let mut out: Vec<u8> = Vec::new();
        SplStreamWriter::new(&mut out).begin_page(&cfg).unwrap();

        assert_eq!(&out[0x5..0x7], &[0xFF, 0xFF], "genişlik kırpıldı");
        assert_eq!(&out[0x7..0x9], &[0xAB, 0xCD], "yükseklik kırpıldı");
        assert_eq!(out.len(), 17);
    }

    /// D-05 regresyonu: QPDL sayfa başlığında `header[0x1]` DİKEY (Y),
    /// `header[0x10]` ise YATAY (X) çözünürlüğü taşır — bu sırada.
    ///
    /// Kaynak, OpenPrinting SpliX qpdl.cpp renderPage:
    ///   header[0x1]  = page->yResolution() / 100;
    ///   header[0x10] = page->xResolution() / 100;
    ///
    /// Sıra sezgiye aykırı olduğu için ("önce Y, sonra X") kolayca ters
    /// yazılabilir. Daha önce tek bir alan vardı ve her iki bayta da YATAY
    /// çözünürlük gidiyordu; simetrik modlarda görünmeyen, `1200x600dpi`'da
    /// sayfayı dikeyde 2 kat ezen bir hataydı.
    #[test]
    fn test_begin_page_maps_resolution_axes_to_correct_bytes() {
        // Asimetrik: X = 1200, Y = 600 (gerçek bir QPDL modu).
        let cfg = PageConfig {
            resolution_x: SplResolution::Dpi1200,
            resolution_y: SplResolution::Dpi600,
            ..PageConfig::default()
        };
        let mut out: Vec<u8> = Vec::new();
        SplStreamWriter::new(&mut out).begin_page(&cfg).unwrap();

        assert_eq!(
            out[0x1], 6,
            "header[0x1] DİKEY (Y) çözünürlük olmalı: 600/100"
        );
        assert_eq!(
            out[0x10], 12,
            "header[0x10] YATAY (X) çözünürlük olmalı: 1200/100"
        );

        // Ters yön: eksenlerin gerçekten ayrı taşındığını kanıtlar.
        let swapped = PageConfig {
            resolution_x: SplResolution::Dpi600,
            resolution_y: SplResolution::Dpi1200,
            ..PageConfig::default()
        };
        let mut out2: Vec<u8> = Vec::new();
        SplStreamWriter::new(&mut out2)
            .begin_page(&swapped)
            .unwrap();
        assert_eq!(out2[0x1], 12);
        assert_eq!(out2[0x10], 6);

        // Simetrik durumda iki bayt eşit (eski davranışla uyum).
        for dpi in [
            SplResolution::Dpi300,
            SplResolution::Dpi600,
            SplResolution::Dpi1200,
        ] {
            let sym = PageConfig {
                resolution_x: dpi,
                resolution_y: dpi,
                ..PageConfig::default()
            };
            let mut o: Vec<u8> = Vec::new();
            SplStreamWriter::new(&mut o).begin_page(&sym).unwrap();
            assert_eq!(o[0x1], o[0x10]);
            assert_eq!(o[0x1] as u32, dpi.dpi() / 100);
        }
    }

    /// QPDL kağıt kaynağı kodları (SpliX printer.cpp) birebir taşınmalı;
    /// `0` "seçilmedi" demektir ve Auto'ya düşer, tanınmayan değerler ise
    /// sessizce bir koda dönüşmek yerine `None` verir.
    #[test]
    fn test_paper_source_from_media_position() {
        assert_eq!(
            SplPaperSource::from_media_position(0),
            Some(SplPaperSource::Auto)
        );
        assert_eq!(
            SplPaperSource::from_media_position(1),
            Some(SplPaperSource::Auto)
        );
        assert_eq!(
            SplPaperSource::from_media_position(2),
            Some(SplPaperSource::Manual)
        );
        assert_eq!(
            SplPaperSource::from_media_position(3),
            Some(SplPaperSource::Multi)
        );
        assert_eq!(
            SplPaperSource::from_media_position(4),
            Some(SplPaperSource::Upper)
        );
        assert_eq!(
            SplPaperSource::from_media_position(5),
            Some(SplPaperSource::Lower)
        );
        for unknown in [6u32, 7, 99, u32::MAX] {
            assert_eq!(
                SplPaperSource::from_media_position(unknown),
                None,
                "{}",
                unknown
            );
        }
        // Kodlar QPDL sayfa başlığına ham olarak yazıldığı için sayısal
        // değerleri de sabitlensin.
        assert_eq!(SplPaperSource::Auto as u8, 1);
        assert_eq!(SplPaperSource::Manual as u8, 2);
    }

    /// PJL sözlüğü dışında hiçbir şey `PAPERTYPE` değerine dönüşemez.
    #[test]
    fn test_pjl_paper_type_only_accepts_printer_vocabulary() {
        for known in PJL_PAPER_TYPES {
            assert_eq!(pjl_paper_type(known), Some(known));
            // ASCII büyük/küçük harf farkı geri düşüşe yol açmamalı.
            assert_eq!(pjl_paper_type(&known.to_ascii_lowercase()), Some(known));
            // Baştaki/sondaki boşluklar (64 baytlık C dizesi) tolere edilmeli.
            assert_eq!(pjl_paper_type(&format!("  {}  ", known)), Some(known));
        }
        // Eski, okunabilir PPD adları artık protokol değeri değil.
        for unknown in ["", "Plain", "Envelope", "CardStock", "NORMAL\nEVIL", "EN V"] {
            assert_eq!(pjl_paper_type(unknown), None, "{:?} kabul edildi", unknown);
        }
    }

    /// `PAPERTYPE` satırı TIRNAKSIZDIR: değere yalnızca tablodaki sabitler
    /// girebildiği için satır hiçbir girdiyle bozulamaz.
    #[test]
    fn test_begin_job_emits_papertype_from_config() {
        for paper_type in PJL_PAPER_TYPES {
            let cfg = JobConfig {
                paper_type,
                ..JobConfig::default()
            };
            let mut out: Vec<u8> = Vec::new();
            {
                let mut w = SplStreamWriter::new(&mut out);
                w.begin_job(&cfg).unwrap();
                w.end_job().unwrap();
            }
            let text = String::from_utf8_lossy(&out).into_owned();
            assert!(
                text.contains(&format!("@PJL SET PAPERTYPE={}\n", paper_type)),
                "{} yazılmadı: {}",
                paper_type,
                text
            );
            // Satır sayısı sabit kalmalı: değer hiçbir zaman satır bölmez.
            // Simplex bir işte 12 satır var: SERVICEDATE, USERNAME, JOBNAME,
            // POWERSAVE, POWERSAVETIME, JAMRECOVERY, DUPLEX, PAPERTYPE,
            // ALTITUDE, DENSITY, RET, ENTER LANGUAGE.
            assert_eq!(
                out.iter().filter(|&&b| b == b'\n').count(),
                12,
                "PJL satır sayısı değişti ({})",
                paper_type
            );
        }
    }

    #[test]
    fn test_algo0x11_compression() {
        let mut sample = vec![0x00u8; 620 * 64];
        sample[100..500].fill(0xAA);
        let comp = Algo0x11::compress(&sample).unwrap();
        assert!(comp.len() < sample.len());
    }

    #[test]
    fn test_algo0x11_roundtrip_synthetic() {
        // Rastgele olmayan ama tekrarlı+değişken içerik: metin benzeri veri.
        let mut sample = vec![0u8; 620 * 128];
        for (i, b) in sample.iter_mut().enumerate() {
            *b = ((i * 37 + (i / 620) * 13) % 251) as u8;
            if (i / 620) % 5 == 0 && (i % 620) > 50 && (i % 620) < 400 {
                *b = 0xFF;
            }
        }
        let comp = Algo0x11::compress(&sample).unwrap();
        let decomp = Algo0x11::decompress(&comp);
        assert_eq!(decomp.len(), sample.len(), "decompressed length mismatch");
        assert_eq!(decomp, sample, "decompressed content mismatch");
    }

    #[test]
    fn test_algo0x11_roundtrip_real_band0() {
        use crate::raster::CupsRasterReader;
        use std::fs::File;
        use std::io::BufReader;

        let path = "target/test_output/test.raster";
        let file = match File::open(path) {
            Ok(f) => f,
            Err(_) => {
                eprintln!("SKIP: {} yok, test atlandı", path);
                return;
            }
        };
        let mut reader = CupsRasterReader::new(BufReader::new(file)).unwrap();
        let header = reader.next_page_header().unwrap().unwrap();

        let page_width_pixels =
            (((header.page_size_points[0] as f64 * header.hw_resolution[0] as f64 / 72.0).ceil())
                as u32
                + 7)
                & !7u32;
        let band_width_bytes = ((page_width_pixels + 7) / 8) as usize;
        let cups_line_bytes = header.bytes_per_line as usize;
        let margin_bytes = (band_width_bytes - cups_line_bytes) / 2;
        let bytes_to_copy = cups_line_bytes.min(band_width_bytes - margin_bytes);
        let band_height = 128usize;

        let mut line_buffer = vec![0u8; cups_line_bytes];
        let mut band_data = vec![0u8; band_width_bytes * band_height];
        let mut found_band = None;

        // Sayfanın en üstü genelde boş kenar boşluğudur; gerçek (tekdüze
        // olmayan) içerik barındıran ilk bandı bulana kadar ilerle.
        for band_idx in 0..(header.height as usize / band_height + 1) {
            for b in band_data.iter_mut() {
                *b = 0;
            }
            let mut lines_read = 0;
            for y in 0..band_height {
                if reader.read_line(&mut line_buffer).is_err() {
                    break;
                }
                lines_read += 1;
                let dst = y * band_width_bytes + margin_bytes;
                band_data[dst..dst + bytes_to_copy].copy_from_slice(&line_buffer[..bytes_to_copy]);
            }
            if lines_read == 0 {
                break;
            }
            let mut inverted = band_data.clone();
            for b in &mut inverted {
                *b = !*b;
            }
            if inverted.iter().any(|&b| b != inverted[0]) {
                found_band = Some((band_idx, inverted));
                break;
            }
        }

        let (band_idx, band_data) = found_band.expect("sayfada tekdüze olmayan bant bulunamadı");
        eprintln!("Test icin secilen band: {}", band_idx);

        let comp = Algo0x11::compress(&band_data).unwrap();
        let decomp = Algo0x11::decompress(&comp);
        assert_eq!(
            decomp.len(),
            band_data.len(),
            "decompressed length mismatch: {} != {}",
            decomp.len(),
            band_data.len()
        );
        let first_diff = decomp
            .iter()
            .zip(band_data.iter())
            .position(|(a, b)| a != b);
        assert_eq!(
            first_diff, None,
            "decompressed content differs from original at byte offset {:?}",
            first_diff
        );
    }
}
