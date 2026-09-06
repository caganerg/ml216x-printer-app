//! # CUPS Raster Parser
//!
//! Bu modül, Linux CUPS filtre zincirinden (`cups-filters` / `libcupsfilters`)
//! gelen standart CUPS Raster akışını ayrıştırır.
//!
//! Not: PWG Raster (`PwgR` magic) yerine klasik CUPS Raster (`RaSt`, `RaS2`, `RaS3`)
//! ve `cups_page_header2_t` (1796 bayt) veri yapısı esas alınmıştır.
//!
//! Üç sürüm de desteklenir. v1 ve v3 sayfa verisini sıkıştırmasız taşır;
//! v2 (`RaS2`/`2SaR`, yani PWG Raster) satır-RLE kullanır ve
//! `CupsLineDecoder` tarafından şeffaf biçimde çözülür. Çağıran taraf her
//! durumda `CupsRasterReader::read_line` kullanır ve farkı görmez.

use std::io::{self, Read};

// Moved to `geometry` during the crate split: the PAPPL front end needs these
// two types, and `geometry` is compiled whether or not `golden-replay` is.
pub use crate::geometry::{CupsColorOrder, CupsColorSpace};

/// CUPS Raster spesifikasyonuna ait senkronizasyon (magic) baytları.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CupsRasterVersion {
    /// CUPS Raster Sürüm 1 - Big Endian (`RaSt`)
    V1Be,
    /// CUPS Raster Sürüm 1 - Little Endian (`tSaR`)
    V1Le,
    /// CUPS Raster Sürüm 2 - Big Endian (`RaS2`)
    V2Be,
    /// CUPS Raster Sürüm 2 - Little Endian (`2SaR`)
    V2Le,
    /// CUPS Raster Sürüm 3 - Big Endian (`RaS3`)
    V3Be,
    /// CUPS Raster Sürüm 3 - Little Endian (`3SaR`)
    V3Le,
}

impl CupsRasterVersion {
    /// Akışın Big-Endian olup olmadığını döner.
    #[inline]
    pub fn is_big_endian(&self) -> bool {
        matches!(
            self,
            CupsRasterVersion::V1Be | CupsRasterVersion::V2Be | CupsRasterVersion::V3Be
        )
    }

    /// Başlık yapısının beklenen bayt boyutu.
    ///
    /// V1 = 420 bayt (`cups_page_header_t`; son alan `cupsRowStep` 416..420),
    /// V2/V3 = 1796 bayt (`cups_page_header2_t`; son alan `cupsPageSizeName`
    /// 1732..1796). Bu değerler CUPS'un kendi `<cups/raster.h>` başlığına karşı
    /// `sizeof` ile ölçüldü.
    ///
    /// V1 için daha önce kullanılan 436, gerçek yapıdan 16 bayt fazlaydı: her
    /// sayfa başlığı okumasında piksel verisinden fazladan 16 bayt yutuluyor,
    /// böylece ilk sayfa kaymış olarak basılıyor ve sonraki sayfa başlığı
    /// tamamen yanlış ofsetten ayrıştırılıyordu (`Geçersiz cupsBytesPerLine
    /// değeri: 0`).
    #[inline]
    pub fn header_size(&self) -> usize {
        match self {
            CupsRasterVersion::V1Be | CupsRasterVersion::V1Le => 420,
            _ => 1796,
        }
    }

    /// Akışın SAYFA VERİSİNİN CUPS satır-RLE'si ile sıkıştırılmış olup
    /// olmadığını döner.
    ///
    /// CUPS Raster v2 (`RaS2`/`2SaR`) sayfa verisini satır bazlı bir RLE ile
    /// sıkıştırır: `<cups/raster.h>` içinde `CUPS_RASTER_SYNC_PWG` doğrudan
    /// `CUPS_RASTER_SYNCv2`'ye eşitlenmiştir ve PWG Raster tanımı gereği
    /// sıkıştırılmıştır. v1 ve v3 sıkıştırmasızdır. libcups ile üretilen aynı
    /// sayfa (620 B/satır x 200 satır = 124.000 bayt ham veri): `3SaR` dosyası
    /// 125.800 bayt, `2SaR`/`RaS2` dosyası 1.811 bayt.
    #[inline]
    pub fn is_compressed(&self) -> bool {
        matches!(self, CupsRasterVersion::V2Be | CupsRasterVersion::V2Le)
    }
}

/// `cups_page_header2_t` (CUPS V2 / V3) ve `cups_page_header_t` (CUPS V1)
/// sayfa başlığı yapısının Rust modellemesi.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PageHeader {
    // CUPS V1 Başlık Alanları (0..420 bayt)
    pub media_class: String,
    pub media_color: String,
    pub media_type: String,
    pub output_type: String,

    pub advance_distance: u32,
    pub advance_media: u32,
    pub collate: bool,
    pub cut_media: u32,
    pub duplex: bool,
    pub hw_resolution: [u32; 2],        // [X DPI, Y DPI]
    pub imaging_bounding_box: [u32; 4], // [Left, Bottom, Right, Top] (pt)
    pub insert_sheet: bool,
    pub jog: u32,
    pub leading_edge: u32,
    pub margins: [u32; 2], // [Left, Bottom] (pt)
    pub manual_feed: bool,
    pub media_position: u32,
    pub media_weight: u32,
    pub mirror_print: bool,
    pub negative_print: bool,
    pub num_copies: u32,
    pub orientation: u32,
    pub output_face_up: bool,
    pub page_size_points: [u32; 2], // [Width, Length] (1/72 inch points)
    pub separations: bool,
    pub tray_switch: bool,
    /// `Tumble` — CUPS'un çift taraflı baskıda BAĞLAMA KENARINI bildirdiği
    /// alan: `false` = uzun kenar (DuplexNoTumble), `true` = kısa kenar
    /// (DuplexTumble).
    ///
    /// Bu alan daha önce `turn_off` adıyla ayrıştırılıyordu; öyle bir CUPS
    /// alanı yok. `<cups/raster.h>` içindeki `cups_page_header_t`'de
    /// `cupsWidth`'ten hemen önce gelen (yani 368. bayttaki) alan `Tumble`'dır.
    /// Ofset zaten doğruydu, yalnızca ad yanlıştı — ve alan kullanılmadığı için
    /// fark edilmiyordu.
    ///
    /// DİKKAT: buradaki `tumble` ile QPDL sayfa başlığındaki `tumble` baytı
    /// AYNI ŞEY DEĞİLDİR. Bu alan bağlama kenarını seçer; QPDL'deki bayt ise
    /// sayfanın hangi yüze bastığını gösterir ve SpliX'te sayfa numarasının
    /// paritesinden hesaplanır (bkz. spl.rs `begin_page`).
    pub tumble: bool,

    pub width: u32,  // cupsWidth (piksel)
    pub height: u32, // cupsHeight (piksel)
    pub cups_media_type: u32,
    pub bits_per_color: u32,         // cupsBitsPerColor (1, 8, 16)
    pub bits_per_pixel: u32,         // cupsBitsPerPixel (1, 8, 24, 32)
    pub bytes_per_line: u32,         // cupsBytesPerLine
    pub color_order: CupsColorOrder, // cupsColorOrder
    pub color_space: CupsColorSpace, // cupsColorSpace
    pub compression: u32,            // cupsCompression (0 = uncompressed)
    pub row_count: u32,
    pub row_feed: u32,
    pub row_step: u32,

    // CUPS V2 / V3 Genişletilmiş Alanlar (420..1796 bayt)
    pub num_colors: u32,
    pub page_size_f: [f32; 2],
    pub rendering_intent: Option<String>,
    pub page_size_name: Option<String>,
}

impl PageHeader {
    fn parse_c_string(bytes: &[u8]) -> String {
        let len = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        String::from_utf8_lossy(&bytes[..len]).trim().to_string()
    }

    #[inline]
    fn read_u32(buf: &[u8], offset: usize, is_be: bool) -> u32 {
        let slice: [u8; 4] = buf[offset..offset + 4]
            .try_into()
            .expect("Hatalı dilim uzunluğu");
        if is_be {
            u32::from_be_bytes(slice)
        } else {
            u32::from_le_bytes(slice)
        }
    }

    #[inline]
    fn read_f32(buf: &[u8], offset: usize, is_be: bool) -> f32 {
        let slice: [u8; 4] = buf[offset..offset + 4]
            .try_into()
            .expect("Hatalı dilim uzunluğu");
        if is_be {
            f32::from_be_bytes(slice)
        } else {
            f32::from_le_bytes(slice)
        }
    }

    /// CUPS Raster sayfa başlığını ayrıştırır.
    pub fn parse(buf: &[u8], version: CupsRasterVersion) -> io::Result<Self> {
        let is_be = version.is_big_endian();
        let expected_size = version.header_size();

        if buf.len() < expected_size {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "CUPS Raster başlık boyutu yetersiz (beklenen: {} bayt, gelen: {} bayt)",
                    expected_size,
                    buf.len()
                ),
            ));
        }

        // C dize alanları (0..256)
        let media_class = Self::parse_c_string(&buf[0..64]);
        let media_color = Self::parse_c_string(&buf[64..128]);
        let media_type = Self::parse_c_string(&buf[128..192]);
        let output_type = Self::parse_c_string(&buf[192..256]);

        let advance_distance = Self::read_u32(buf, 256, is_be);
        let advance_media = Self::read_u32(buf, 260, is_be);
        let collate = Self::read_u32(buf, 264, is_be) != 0;
        let cut_media = Self::read_u32(buf, 268, is_be);
        let duplex = Self::read_u32(buf, 272, is_be) != 0;

        let hw_res_x = Self::read_u32(buf, 276, is_be);
        let hw_res_y = Self::read_u32(buf, 280, is_be);

        let img_bbox = [
            Self::read_u32(buf, 284, is_be),
            Self::read_u32(buf, 288, is_be),
            Self::read_u32(buf, 292, is_be),
            Self::read_u32(buf, 296, is_be),
        ];

        let insert_sheet = Self::read_u32(buf, 300, is_be) != 0;
        let jog = Self::read_u32(buf, 304, is_be);
        let leading_edge = Self::read_u32(buf, 308, is_be);
        let margin_x = Self::read_u32(buf, 312, is_be);
        let margin_y = Self::read_u32(buf, 316, is_be);
        let manual_feed = Self::read_u32(buf, 320, is_be) != 0;
        let media_position = Self::read_u32(buf, 324, is_be);
        let media_weight = Self::read_u32(buf, 328, is_be);
        let mirror_print = Self::read_u32(buf, 332, is_be) != 0;
        let negative_print = Self::read_u32(buf, 336, is_be) != 0;
        let num_copies = Self::read_u32(buf, 340, is_be);
        let orientation = Self::read_u32(buf, 344, is_be);
        let output_face_up = Self::read_u32(buf, 348, is_be) != 0;
        let page_sz_w = Self::read_u32(buf, 352, is_be);
        let page_sz_h = Self::read_u32(buf, 356, is_be);
        let separations = Self::read_u32(buf, 360, is_be) != 0;
        let tray_switch = Self::read_u32(buf, 364, is_be) != 0;
        let tumble = Self::read_u32(buf, 368, is_be) != 0;

        let width = Self::read_u32(buf, 372, is_be);
        let height = Self::read_u32(buf, 376, is_be);
        let cups_media_type = Self::read_u32(buf, 380, is_be);
        let bits_per_color = Self::read_u32(buf, 384, is_be);
        let bits_per_pixel = Self::read_u32(buf, 388, is_be);
        let bytes_per_line = Self::read_u32(buf, 392, is_be);
        let color_order_val = Self::read_u32(buf, 396, is_be);
        let color_space_val = Self::read_u32(buf, 400, is_be);
        let compression = Self::read_u32(buf, 404, is_be);
        let row_count = Self::read_u32(buf, 408, is_be);
        let row_feed = Self::read_u32(buf, 412, is_be);
        let row_step = Self::read_u32(buf, 416, is_be);

        // V2 / V3 Genişletilmiş Alanlar
        let (num_colors, page_size_f, rendering_intent, page_size_name) = if expected_size >= 1796 {
            let num_colors = Self::read_u32(buf, 420, is_be);
            let ps_w_f = Self::read_f32(buf, 428, is_be);
            let ps_h_f = Self::read_f32(buf, 432, is_be);

            // cupsRenderingIntent: 1668..1732
            let intent_raw = Self::parse_c_string(&buf[1668..1732]);
            let intent = if intent_raw.is_empty() {
                None
            } else {
                Some(intent_raw)
            };

            // cupsPageSizeName: 1732..1796
            let name_raw = Self::parse_c_string(&buf[1732..1796]);
            let name = if name_raw.is_empty() {
                None
            } else {
                Some(name_raw)
            };

            (num_colors, [ps_w_f, ps_h_f], intent, name)
        } else {
            (0, [page_sz_w as f32, page_sz_h as f32], None, None)
        };

        Ok(Self {
            media_class,
            media_color,
            media_type,
            output_type,
            advance_distance,
            advance_media,
            collate,
            cut_media,
            duplex,
            hw_resolution: [hw_res_x, hw_res_y],
            imaging_bounding_box: img_bbox,
            insert_sheet,
            jog,
            leading_edge,
            margins: [margin_x, margin_y],
            manual_feed,
            media_position,
            media_weight,
            mirror_print,
            negative_print,
            num_copies,
            orientation,
            output_face_up,
            page_size_points: [page_sz_w, page_sz_h],
            separations,
            tray_switch,
            tumble,
            width,
            height,
            cups_media_type,
            bits_per_color,
            bits_per_pixel,
            bytes_per_line,
            color_order: CupsColorOrder::from(color_order_val),
            color_space: CupsColorSpace(color_space_val),
            compression,
            row_count,
            row_feed,
            row_step,
            num_colors,
            page_size_f,
            rendering_intent,
            page_size_name,
        })
    }

    /// Sayfaya ait ham (sıkıştırılmamış) toplam piksel verisi boyutu.
    #[inline]
    pub fn total_raster_bytes(&self) -> u64 {
        (self.bytes_per_line as u64) * (self.height as u64)
    }
}

/// Akıştan CUPS Raster verisini okuyan ayrıştırıcı.
pub struct CupsRasterReader<R: Read> {
    reader: R,
    version: CupsRasterVersion,
    page_count: u32,
    /// v2 akışlarında satır çözücüsünün sayfa başına durumu; v1/v3'te `None`.
    decoder: Option<CupsLineDecoder>,
}

impl<R: Read> CupsRasterReader<R> {
    /// 4 baytlık CUPS Raster senkronizasyon kodunu doğrulayarak okuyucuyu başlatır.
    pub fn new(mut reader: R) -> io::Result<Self> {
        let mut magic = [0u8; 4];
        if let Err(e) = reader.read_exact(&mut magic) {
            if e.kind() == io::ErrorKind::UnexpectedEof {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "Girdi akışı boş (0 bayt). cupsfilter veya önceki filtre aşamasının başarıyla raster ürettiğinden emin olun.",
                ));
            }
            return Err(e);
        }

        let version = match &magic {
            b"RaSt" => CupsRasterVersion::V1Be,
            b"tSaR" => CupsRasterVersion::V1Le,
            b"RaS2" => CupsRasterVersion::V2Be,
            b"2SaR" => CupsRasterVersion::V2Le,
            b"RaS3" => CupsRasterVersion::V3Be,
            b"3SaR" => CupsRasterVersion::V3Le,
            other => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "Geçersiz CUPS Raster formatı: {:?} (Beklenen: 'RaSt', 'tSaR', 'RaS2', '2SaR', 'RaS3', '3SaR')",
                        String::from_utf8_lossy(other)
                    ),
                ));
            }
        };

        Ok(Self {
            reader,
            version,
            page_count: 0,
            decoder: None,
        })
    }

    #[inline]
    pub fn version(&self) -> CupsRasterVersion {
        self.version
    }

    #[allow(dead_code)]
    #[inline]
    pub fn page_count(&self) -> u32 {
        self.page_count
    }

    /// Sonraki sayfa başlığını okur. Akış, sayfalar arasında temiz bir şekilde
    /// (0 bayt okunarak) sona ererse `Ok(None)` döner.
    ///
    /// `read_exact` tek başına, akışın başlık ortasında kesildiği (bozuk/yarım
    /// veri) durumu ile iki sayfa arasındaki normal akış sonunu AYIRT EDEMEZ;
    /// ikisi de aynı `UnexpectedEof` hatasını üretir. Bu, gerçek bir bozulmayı
    /// sessizce "işi normal bitir" olarak yorumlamamak için baytları elle,
    /// sayarak okur.
    pub fn next_page_header(&mut self) -> io::Result<Option<PageHeader>> {
        let header_len = self.version.header_size();
        let mut buf = vec![0u8; header_len];

        let mut total_read = 0usize;
        while total_read < header_len {
            match self.reader.read(&mut buf[total_read..]) {
                Ok(0) => break,
                Ok(n) => total_read += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e),
            }
        }

        if total_read == 0 {
            return Ok(None);
        }
        if total_read < header_len {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "CUPS Raster akışı sayfa başlığının ortasında kesildi \
                     ({} / {} bayt okundu). Önceki filtre aşaması yarıda kesilmiş olabilir.",
                    total_read, header_len
                ),
            ));
        }

        self.page_count += 1;
        let header = PageHeader::parse(&buf, self.version)?;

        // Sıkıştırma durumu SAYFA başına sıfırlanır: bir sayfadan artakalan
        // satır tekrar sayacı sonraki sayfaya sızmamalı.
        self.decoder = if self.version.is_compressed() {
            Some(CupsLineDecoder::new(&header))
        } else {
            None
        };

        Ok(Some(header))
    }

    /// Sayfa raster verisinden TEK BİR SATIR okur ve `out`'u tamamen doldurur.
    ///
    /// Sıkıştırmasız akışlarda (v1/v3) bu düz bir `read_exact`'tir. v2/PWG
    /// akışlarında satır, CUPS'un PackBits türevi satır-RLE'siyle kodlanmıştır
    /// ve burada çözülür; çağıran taraf farkı görmez.
    ///
    /// `out`'un uzunluğu her çağrıda `cupsBytesPerLine` olmalıdır. Çözücü
    /// kendi tamponunu bu uzunluktan boyutlandırır — başlıktaki (henüz
    /// doğrulanmamış, güvenilmez) `bytes_per_line` alanından DEĞİL. Böylece
    /// bozuk bir başlık, `validate_page_header` daha çalışmadan devasa bir
    /// tahsis tetikleyemez.
    pub fn read_line(&mut self, out: &mut [u8]) -> io::Result<()> {
        match &mut self.decoder {
            None => self.reader.read_exact(out),
            Some(decoder) => decoder.read_line(&mut self.reader, out),
        }
    }
}

/// CUPS Raster v2 (`RaS2`/`2SaR`, PWG Raster ile aynı) satır-RLE çözücüsü.
///
/// Kodlama, satır başına şu yapıdadır:
///
/// ```text
/// [tekrar]  : bu satır (tekrar + 1) kez yinelenir
/// ardından cupsBytesPerLine dolana kadar:
///   n == 128 : satırın sonuna kadar boş renkle doldur
///   n >  128 : (257 - n) pikselin ham (literal) kopyası gelir
///   n <  128 : sonraki tek piksel (n + 1) kez yinelenir
/// ```
///
/// Kodlama, libcups'un `cupsRasterWritePixels` çıktısına karşı bire bir
/// doğrulandı: 620 bayt sıfırdan oluşan 200 satırlık bir sayfa
/// `c7 7f 00 7f 00 7f 00 7f 00 6b 00` (11 bayt) olarak kodlanıyor —
/// yani `[199]` + `[127, 0x00] x4` + `[107, 0x00]` = 200 satır x 620 bayt.
struct CupsLineDecoder {
    /// Piksel başına bayt. `cupsBitsPerPixel < 8` için libcups gibi 1 kabul
    /// edilir; tekrar ve ham kopya sayaçları PİKSEL cinsindendir, bayt değil.
    bpp: usize,
    /// `n == 128` (satır sonuna kadar boşalt) durumunda kullanılan dolgu.
    ///
    /// libcups, toner/mürekkep ekleyen renk uzaylarında (K, CMY, CMYK, White,
    /// Gold, Silver) boşluğu `0x00`, diğerlerinde `0xFF` ile doldurur.
    blank_fill: u8,
    /// Son çözülen satırın kaç kez daha tekrarlanacağı.
    repeat_remaining: u32,
    /// Son çözülen satır; tekrarlar buradan kopyalanır. İlk `read_line`
    /// çağrısında, çağıranın verdiği tampon uzunluğundan boyutlandırılır.
    last_line: Vec<u8>,
}

impl CupsLineDecoder {
    fn new(header: &PageHeader) -> Self {
        // libcups cups_raster_update(): 8 bitten küçük derinliklerde bpp 1'dir.
        let bpp = if header.bits_per_pixel >= 8 {
            (header.bits_per_pixel as usize).div_ceil(8)
        } else {
            1
        };

        Self {
            bpp: bpp.max(1),
            blank_fill: header.color_space.blank_fill(),
            repeat_remaining: 0,
            last_line: Vec::new(),
        }
    }

    fn read_line<R: Read>(&mut self, reader: &mut R, out: &mut [u8]) -> io::Result<()> {
        if out.is_empty() {
            return Ok(());
        }

        // Tampon ilk kullanımda çağıranın uzunluğundan kurulur; sonraki
        // çağrılarda uzunluk değişemez (sayfa ortasında değişmesi, çağıranın
        // bir hatası olurdu ve sessizce yanlış çözmektense hata verilir).
        if self.last_line.is_empty() {
            self.last_line = vec![0u8; out.len()];
        } else if self.last_line.len() != out.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "Satır uzunluğu sayfa ortasında değişti: {} -> {}",
                    self.last_line.len(),
                    out.len()
                ),
            ));
        }

        if self.repeat_remaining > 0 {
            self.repeat_remaining -= 1;
            out.copy_from_slice(&self.last_line);
            return Ok(());
        }

        let repeat = read_u8(reader)?;
        self.repeat_remaining = repeat as u32;
        self.decode_line(reader)?;
        out.copy_from_slice(&self.last_line);
        Ok(())
    }

    fn decode_line<R: Read>(&mut self, reader: &mut R) -> io::Result<()> {
        let line_len = self.last_line.len();
        let bpp = self.bpp;
        let mut pos = 0usize;

        while pos < line_len {
            let n = read_u8(reader)?;

            if n == 128 {
                // Satırın sonuna kadar boş renkle doldur.
                self.last_line[pos..].fill(self.blank_fill);
                return Ok(());
            }

            if n > 128 {
                // (257 - n) piksellik ham kopya: n=255 -> 2, n=129 -> 128.
                let count = (257 - n as usize).checked_mul(bpp).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::InvalidData,
                        "CUPS v2 literal kayıt uzunluğu taşma oluşturdu",
                    )
                })?;
                if count > line_len - pos {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        format!(
                            "CUPS v2 literal kayıt satır sınırını aşıyor: kalan {} bayt, kayıt {} bayt",
                            line_len - pos,
                            count
                        ),
                    ));
                }
                reader.read_exact(&mut self.last_line[pos..pos + count])?;
                pos += count;
                continue;
            }

            // Sonraki tek piksel (n + 1) kez yinelenir.
            let count = (n as usize + 1).checked_mul(bpp).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "CUPS v2 tekrar kayıt uzunluğu taşma oluşturdu",
                )
            })?;
            if count > line_len - pos {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "CUPS v2 tekrar kaydı satır sınırını aşıyor: kalan {} bayt, kayıt {} bayt",
                        line_len - pos,
                        count
                    ),
                ));
            }

            reader.read_exact(&mut self.last_line[pos..pos + bpp])?;
            let (written, rest) = self.last_line.split_at_mut(pos + bpp);
            let pixel = &written[pos..pos + bpp];
            let repeats = count / bpp - 1;
            for chunk in rest.chunks_mut(bpp).take(repeats) {
                chunk.copy_from_slice(pixel);
            }
            pos += count;
        }

        Ok(())
    }
}

#[inline]
fn read_u8<R: Read>(reader: &mut R) -> io::Result<u8> {
    let mut b = [0u8; 1];
    reader.read_exact(&mut b)?;
    Ok(b[0])
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    #[test]
    fn test_cups_sync_words() {
        // Altı sürüm de kabul edilir; v2 (`RaS2`/`2SaR`) sayfa verisini
        // satır-RLE ile taşır ve `CupsLineDecoder` tarafından şeffaf biçimde
        // çözülür (bkz. main.rs `test_v2_and_v3_streams_produce_identical_output`).
        // Burada temsilci olarak bir v3 ve bir v1 akışı üzerinden sürüm/endian
        // eşlemesini doğruluyoruz, v2'nin bayrakları ise aşağıda ayrıca.
        let v3_be = b"RaS3";
        let reader = CupsRasterReader::new(Cursor::new(v3_be)).unwrap();
        assert_eq!(reader.version(), CupsRasterVersion::V3Be);
        assert!(reader.version().is_big_endian());
        assert_eq!(reader.version().header_size(), 1796);

        let v1_le = b"tSaR";
        let reader_v1 = CupsRasterReader::new(Cursor::new(v1_le)).unwrap();
        assert_eq!(reader_v1.version(), CupsRasterVersion::V1Le);
        assert!(!reader_v1.version().is_big_endian());
        assert_eq!(reader_v1.version().header_size(), 420);

        // Sürüm eşlemesi, akışı reddetmekten bağımsız olarak doğru kalmalı.
        assert_eq!(CupsRasterVersion::V2Be.header_size(), 1796);
        assert!(CupsRasterVersion::V2Be.is_compressed());
        assert!(CupsRasterVersion::V2Le.is_compressed());
    }

    #[test]
    fn test_cups_header_parse_canonical() {
        let mut header_buf = vec![0u8; 1796];

        // HWResolution: [600, 600]
        header_buf[276..280].copy_from_slice(&600u32.to_be_bytes());
        header_buf[280..284].copy_from_slice(&600u32.to_be_bytes());

        // PageSize points: [595, 842] (A4)
        header_buf[352..356].copy_from_slice(&595u32.to_be_bytes());
        header_buf[356..360].copy_from_slice(&842u32.to_be_bytes());

        // width: 4960, height: 7016
        header_buf[372..376].copy_from_slice(&4960u32.to_be_bytes());
        header_buf[376..380].copy_from_slice(&7016u32.to_be_bytes());

        // bits_per_color: 1, bits_per_pixel: 1, bytes_per_line: 620
        header_buf[384..388].copy_from_slice(&1u32.to_be_bytes());
        header_buf[388..392].copy_from_slice(&1u32.to_be_bytes());
        header_buf[392..396].copy_from_slice(&620u32.to_be_bytes());

        // color_space: 3 (K/Black)
        header_buf[400..404].copy_from_slice(&3u32.to_be_bytes());

        // cupsPageSizeName at offset 1732: "A4"
        header_buf[1732..1734].copy_from_slice(b"A4");

        let header = PageHeader::parse(&header_buf, CupsRasterVersion::V2Be).unwrap();
        assert_eq!(header.hw_resolution, [600, 600]);
        assert_eq!(header.page_size_points, [595, 842]);
        assert_eq!(header.width, 4960);
        assert_eq!(header.height, 7016);
        assert_eq!(header.bits_per_color, 1);
        assert_eq!(header.bits_per_pixel, 1);
        assert_eq!(header.bytes_per_line, 620);
        assert_eq!(header.color_space, CupsColorSpace::K);
        assert_eq!(header.page_size_name.as_deref(), Some("A4"));
        assert_eq!(header.total_raster_bytes(), 620 * 7016);
    }

    /// V1 başlık boyutu, CUPS'un `cups_page_header_t` yapısının gerçek
    /// boyutuyla (420 bayt) eşleşmeli. Daha önce kullanılan 436, her başlıkta
    /// piksel verisinden 16 bayt yutup akışı kaydırıyordu.
    #[test]
    fn test_v1_header_size_matches_cups_struct() {
        assert_eq!(CupsRasterVersion::V1Be.header_size(), 420);
        assert_eq!(CupsRasterVersion::V1Le.header_size(), 420);
        // V2/V3 (`cups_page_header2_t`) değişmedi.
        assert_eq!(CupsRasterVersion::V3Be.header_size(), 1796);
    }

    /// Y-02 regresyonu: ardışık V1 sayfaları akışta senkron kalmalı.
    ///
    /// Başlık boyutu bir bayt bile fazla okunursa 2. sayfanın alanları yanlış
    /// ofsetten ayrıştırılır; 436 ile bu test `bytes_per_line == 0` üretiyordu.
    #[test]
    fn test_v1_stream_stays_in_sync_across_pages() {
        const V1_HEADER_LEN: usize = 420;

        let mut header = vec![0u8; V1_HEADER_LEN];
        let mut put = |off: usize, val: u32| {
            header[off..off + 4].copy_from_slice(&val.to_be_bytes());
        };
        put(276, 600); // hw_resolution[0]
        put(280, 600); // hw_resolution[1]
        put(352, 595); // page_size_points[0]
        put(356, 842); // page_size_points[1]
        put(372, 32); // width
        put(376, 3); // height
        put(384, 1); // bits_per_color
        put(388, 1); // bits_per_pixel
        put(392, 4); // bytes_per_line = ceil(32 * 1 / 8)
        put(400, 3); // color_space = K
        put(416, 0xABCD); // row_step: 420 sınırındaki SON alan

        let pixels = vec![0u8; 4 * 3];
        let mut stream = b"RaSt".to_vec();
        for _ in 0..2 {
            stream.extend_from_slice(&header);
            stream.extend_from_slice(&pixels);
        }

        let mut reader = CupsRasterReader::new(Cursor::new(stream)).unwrap();
        for page in 1..=2 {
            let h = reader
                .next_page_header()
                .unwrap()
                .unwrap_or_else(|| panic!("sayfa {} başlığı okunamadı", page));
            assert_eq!(h.bytes_per_line, 4, "sayfa {} kaymış", page);
            assert_eq!(h.width, 32, "sayfa {} kaymış", page);
            assert_eq!(h.height, 3, "sayfa {} kaymış", page);
            assert_eq!(h.row_step, 0xABCD, "sayfa {} son alanı kaymış", page);

            // Sayfa verisini tüket ki sıradaki başlık doğru ofsetten okunsun.
            let mut line = vec![0u8; 4];
            for _ in 0..3 {
                reader.read_line(&mut line).unwrap();
            }
        }
        assert!(
            reader.next_page_header().unwrap().is_none(),
            "akış temiz bitmeli"
        );
    }

    /// Altı sync sözcüğü de kabul edilmeli; sıkıştırma bayrağı doğru kurulmalı.
    #[test]
    fn test_accepts_all_known_sync_words() {
        for (magic, compressed) in [
            (b"RaSt", false),
            (b"tSaR", false),
            (b"RaS2", true),
            (b"2SaR", true),
            (b"RaS3", false),
            (b"3SaR", false),
        ] {
            let reader = CupsRasterReader::new(Cursor::new(magic))
                .unwrap_or_else(|e| panic!("{:?} reddedildi: {}", magic, e));
            assert_eq!(reader.version().is_compressed(), compressed, "{:?}", magic);
        }
    }

    /// 1 bayt/piksel için tek satırlık bir v2 akışı kurar.
    fn v2_page(bytes_per_line: u32, height: u32, payload: &[u8]) -> Vec<u8> {
        let mut hdr = vec![0u8; 1796];
        let mut put = |off: usize, val: u32| {
            hdr[off..off + 4].copy_from_slice(&val.to_be_bytes());
        };
        put(276, 600);
        put(280, 600);
        put(352, 595);
        put(356, 842);
        put(372, bytes_per_line * 8);
        put(376, height);
        put(384, 1);
        put(388, 1);
        put(392, bytes_per_line);
        put(400, 3); // K

        let mut stream = b"RaS2".to_vec();
        stream.extend_from_slice(&hdr);
        stream.extend_from_slice(payload);
        stream
    }

    fn decode_v2(bytes_per_line: u32, height: u32, payload: &[u8]) -> Vec<Vec<u8>> {
        let mut reader =
            CupsRasterReader::new(Cursor::new(v2_page(bytes_per_line, height, payload))).unwrap();
        reader.next_page_header().unwrap().unwrap();
        (0..height)
            .map(|_| {
                let mut line = vec![0u8; bytes_per_line as usize];
                reader.read_line(&mut line).unwrap();
                line
            })
            .collect()
    }

    /// libcups'un ürettiği GERÇEK bayt dizisi çözülebilmeli.
    ///
    /// `cupsRasterWritePixels` ile yazılan, 620 bayt sıfırdan oluşan 200
    /// satırlık bir sayfanın tamamı tam olarak bu 11 bayttır; kodlamayı
    /// bu dosyadan bire bir okudum.
    #[test]
    fn test_v2_decodes_real_libcups_payload() {
        let payload = [
            0xc7, 0x7f, 0x00, 0x7f, 0x00, 0x7f, 0x00, 0x7f, 0x00, 0x6b, 0x00,
        ];
        let lines = decode_v2(620, 200, &payload);
        assert_eq!(lines.len(), 200);
        for (i, line) in lines.iter().enumerate() {
            assert!(line.iter().all(|&b| b == 0), "satır {} sıfır değil", i);
        }
    }

    /// Üç kayıt türü de doğru çözülmeli: tekrar, ham kopya, satır sonuna
    /// kadar boşaltma.
    #[test]
    fn test_v2_decodes_each_record_kind() {
        // [0] satır tekrarı yok
        // [2, 0xAB]      -> 0xAB x3
        // [0xFE, 1, 2, 3] -> (257-254)=3 ham bayt
        // [128]          -> satır sonuna kadar boş (K => 0x00)
        let payload = [0x00, 0x02, 0xAB, 0xFE, 0x01, 0x02, 0x03, 0x80];
        let lines = decode_v2(8, 1, &payload);
        assert_eq!(lines[0], vec![0xAB, 0xAB, 0xAB, 1, 2, 3, 0x00, 0x00]);
    }

    /// Satır tekrar sayacı: `[n]` başlığı satırı (n + 1) kez üretir.
    #[test]
    fn test_v2_line_repeat_count() {
        // [3] -> 4 satır; her satır [0,0xF0] + [128] ile 0xF0 sonra sıfırlar
        let payload = [0x03, 0x00, 0xF0, 0x80];
        let lines = decode_v2(4, 4, &payload);
        assert_eq!(lines.len(), 4);
        for line in &lines {
            assert_eq!(line, &vec![0xF0, 0x00, 0x00, 0x00]);
        }
    }

    /// Satır tekrarı SAYFA sınırını aşmamalı: bir sayfadan artakalan sayaç
    /// sonraki sayfanın ilk satırına sızarsa tüm akış kayar.
    #[test]
    fn test_v2_repeat_state_resets_between_pages() {
        // Her sayfa "10 satır tekrarı" bildiriyor ama sayfa yüksekliği 1.
        let payload = [0x09, 0x00, 0xAA, 0x80];
        let mut stream = v2_page(4, 1, &payload);
        let second = v2_page(4, 1, &[0x09, 0x00, 0xBB, 0x80]);
        stream.extend_from_slice(&second[4..]); // magic'i atla

        let mut reader = CupsRasterReader::new(Cursor::new(stream)).unwrap();
        let mut line = vec![0u8; 4];

        reader.next_page_header().unwrap().unwrap();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line[0], 0xAA);

        reader.next_page_header().unwrap().unwrap();
        reader.read_line(&mut line).unwrap();
        assert_eq!(line[0], 0xBB, "önceki sayfanın tekrar sayacı sızdı");
    }

    /// Bildirilen satırı aşan sayaçlar kırpılmamalı: kırpma, bir sonraki
    /// kaydın baytlarını kontrol baytı sanıp bütün akışın kaymasına yol açar.
    #[test]
    fn test_v2_oversized_counts_are_rejected() {
        for payload in [
            vec![0x00, 0x7F, 0x5A],
            vec![0x00, 0x81, 0x11, 0x22, 0x33, 0x44],
        ] {
            let mut reader = CupsRasterReader::new(Cursor::new(v2_page(4, 1, &payload))).unwrap();
            reader.next_page_header().unwrap().unwrap();

            let mut line = vec![0u8; 4];
            let err = reader
                .read_line(&mut line)
                .expect_err("satırı aşan v2 kaydı reddedilmeliydi");
            assert_eq!(err.kind(), io::ErrorKind::InvalidData);
            assert!(err.to_string().contains("satır sınırını"), "{}", err);
        }
    }

    /// Yarıda kesilen bir v2 akışı hata döndürmeli, panic atmamalı.
    #[test]
    fn test_v2_truncated_payload_errors_cleanly() {
        let mut reader =
            CupsRasterReader::new(Cursor::new(v2_page(620, 10, &[0x00, 0x7F]))).unwrap();
        reader.next_page_header().unwrap().unwrap();
        let mut line = vec![0u8; 620];
        assert!(reader.read_line(&mut line).is_err());
    }
}
