mod media;
pub mod raster;
pub mod spl;

/// Altın dosya (golden file) koşum takımı; bkz. `src/golden.rs`.
#[cfg(test)]
mod golden;

use std::env;
use std::fs::File;
use std::io::{self, BufReader, Read, Write};
use std::process;

use raster::{CupsColorOrder, CupsColorSpace, CupsRasterReader, PageHeader};
use spl::{
    current_service_date, JobConfig, PageConfig, SplDuplex, SplPaperSize, SplPaperSource,
    SplResolution, SplStreamWriter,
};

/// CUPS Filtre Argümanları
/// Standart çağrı: `filter job-id user title num-copies options [filename]`
///
/// `num_copies` (argv[4]) ve `options` (argv[5]) KASITLI olarak okunmuyor;
/// alanlar yalnızca teşhis ve konum doğruluğu için tutuluyor. Gerekçe: bu bir
/// raster filtresidir ve zincirde kendisinden önce çalışan cups-filters aşaması
/// (`gstoraster`/`pdftoraster`) PPD'yi zaten yorumlayıp seçilen medyayı,
/// çözünürlüğü ve kopya sayısını CUPS Raster SAYFA BAŞLIĞINA yazar. Sayfa
/// başlığı ile komut satırı çeliştiğinde bağlayıcı olan başlıktır — sayfa
/// verisi ona göre üretilmiştir. Buradan seçenek okumak, üretilen veriyle
/// çelişen bir başlık yazmak anlamına gelirdi.
#[allow(dead_code)]
#[derive(Default)]
struct CupsFilterArgs {
    pub job_id: Option<String>,
    pub user: Option<String>,
    pub title: Option<String>,
    pub num_copies: Option<String>,
    pub options: Option<String>,
    pub filename: Option<String>,
}

impl CupsFilterArgs {
    fn parse(args: &[String]) -> Self {
        if args.len() >= 6 {
            Self {
                job_id: Some(args[1].clone()),
                user: Some(args[2].clone()),
                title: Some(args[3].clone()),
                num_copies: Some(args[4].clone()),
                options: Some(args[5].clone()),
                filename: args.get(6).cloned(),
            }
        } else if args.len() == 2 && !args[1].starts_with('-') {
            // Doğrudan dosya modu: `cargo run -- dosya.raster`
            Self {
                filename: Some(args[1].clone()),
                ..Self::default()
            }
        } else {
            Self::default()
        }
    }
}

fn main() {
    // `env::args()` argv'de geçersiz UTF-8 baytı bulunca panic atar; ama bu
    // argümanlar (`job-id user title copies options [file]`) `cupsd`
    // tarafından işi gönderen istemcinin verdiği alanlardan (ör. job-name)
    // türetiliyor ve güvenilmez kabul edilmeli. Bozuk/kötü niyetli bir başlık
    // filtreyi daha ilk argümanı okurken çökertip DoS'a yol açmasın diye
    // `env::args_os()` + kayıplı (lossy) UTF-8 dönüşümü kullanılıyor: geçersiz
    // baytlar sessizce `U+FFFD` ile değiştirilir, panic olmaz.
    let raw_args: Vec<String> = env::args_os()
        .map(|s| s.to_string_lossy().into_owned())
        .collect();

    // CUPS filtreleri normalde `filter job-id user title copies options [file]`
    // (5-6 argüman) ile çağrılır. Programın hiç argümansız (yalnızca ikili
    // dosya adıyla) çalıştırılması gerçek bir `cupsd` çağrısı değildir — ya
    // yanlış kurulumdur ya da elle/keşif amaçlı bir çalıştırmadır. Bu araca
    // en yakın mimari eşdeğerler olan `/usr/lib/cups/filter/rastertopwg` ve
    // `pstops` çalıştırılarak doğrulandı: ikisi de bu durumda bir
    // "Usage: ..." mesajı basıp 1 koduyla çıkıyor; CUPS *backend*'lerine
    // özgü olan "desteklenen MIME türlerini listeleyip 0 ile çık" davranışını
    // UYGULAMIYORLAR (o davranış filtreler için değil, aygıt keşfi yapan
    // backend'ler içindir). Bu yüzden burada da aynı yaklaşım izleniyor: boş
    // stdin okumayı denemeden önce net bir kullanım mesajıyla erken çıkılıyor.
    if raw_args.len() <= 1 {
        let prog = raw_args
            .first()
            .cloned()
            .unwrap_or_else(|| "rastertospl-rust".to_string());
        eprintln!("Usage: {} job-id user title copies options [file]", prog);
        process::exit(1);
    }

    let args = CupsFilterArgs::parse(&raw_args);

    // `user`, `title` ve `job_id`, işi gönderen istemciden (CUPS üzerinden)
    // geldiği için güvenilmez kabul edilmeli: `{}` yerine `{:?}` (Debug)
    // kullanmak, gömülü ANSI/terminal kaçış dizilerini ve kontrol
    // karakterlerini (ör. ESC, CR) `\u{1b}` gibi kaçırılmış (escaped)
    // biçimde yazdırarak sahte log satırı enjeksiyonunu ya da terminal
    // emülatörü zafiyetlerinin tetiklenmesini önler. Normal akışta `job_id`
    // sayısal bir dize olsa da, filtre manipüle edilmiş argümanlarla elle
    // çağrıldığında bu garanti yoktur.
    if let (Some(job), Some(user)) = (&args.job_id, &args.user) {
        eprintln!("DEBUG: CUPS Job ID: {:?}, User: {:?}", job, user);
    }
    if let Some(title) = &args.title {
        eprintln!("DEBUG: CUPS Title: {:?}", title);
    }

    let input_reader: Box<dyn Read> = match &args.filename {
        Some(path) => {
            eprintln!(
                "DEBUG: CUPS Raster dosyadan okunuyor: {}",
                quote_untrusted(path)
            );
            match File::open(path) {
                Ok(file) => Box::new(BufReader::new(file)),
                Err(err) => {
                    eprintln!(
                        "ERROR: Raster dosyası açılamadı {}: {}",
                        quote_untrusted(path),
                        err
                    );
                    process::exit(1);
                }
            }
        }
        None => {
            eprintln!("DEBUG: CUPS Raster standart girdiden (stdin) okunuyor");
            Box::new(BufReader::new(io::stdin()))
        }
    };

    // `process_cups_raster_to_spl` `SplStreamWriter`'ı YEREL olarak tutar:
    // hata `?` ile döndüğünde writer bu satıra gelmeden düşer ve `Drop`
    // uygulaması kapanış UEL'ini yazar. `process::exit` `Drop` çalıştırmadığı
    // için sıralama önemlidir — hata burada, writer çoktan düştükten sonra
    // raporlanır.
    if let Err(err) =
        process_cups_raster_to_spl(&args, input_reader, io::stdout(), &current_service_date())
    {
        eprintln!("ERROR: Raster işleme hatası: {}", err);
        process::exit(1);
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
fn quote_untrusted(value: &str) -> String {
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
const LINE_OVERSHOOT_SLACK_BYTES: u32 = 1;

/// Fiziksel sayfa yüksekliğinin üzerinde kabul edilen yuvarlama payı.
/// Gerekçe için `validate_page_header` içindeki D-02 açıklamasına bakın.
const HEIGHT_OVERSHOOT_SLACK_LINES: u32 = 8;

fn validate_page_header(header: &PageHeader) -> io::Result<()> {
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
fn compute_page_width_pixels(page_size_pt: u32, x_dpi: u32) -> u32 {
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
fn compute_page_height_lines(page_size_pt: u32, y_dpi: u32) -> u32 {
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
fn hard_margin_bytes(margin_pt: f64, x_dpi: u32) -> usize {
    let px = (margin_pt * x_dpi as f64 / 72.0).ceil() as u32;
    (((px + 7) & !7u32) / 8) as usize
}

/// CUPS satırının bant tamponundaki yatay yerleşimi.
///
/// İki alan birlikte tek bir işaretli ofseti temsil eder: `dst_offset`
/// pozitif kaydırma, `src_skip` ise negatif kaydırmadır (satırın solundan
/// atılan baytlar). İkisi aynı anda sıfırdan büyük olamaz.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct BandPlacement {
    /// İçeriğin bant tamponunda başladığı sütun (bayt).
    dst_offset: usize,
    /// CUPS satırının başından atlanacak bayt sayısı.
    src_skip: usize,
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
fn band_placement(
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
fn duplex_mode(header: &PageHeader) -> SplDuplex {
    if !header.duplex {
        SplDuplex::Simplex
    } else if header.tumble {
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
fn pjl_paper_type_for(header: &PageHeader) -> &'static str {
    if header.media_type.is_empty() {
        return spl::PJL_PAPERTYPE_DEFAULT;
    }
    match spl::pjl_paper_type(&header.media_type) {
        Some(paper_type) => paper_type,
        None => {
            // `MediaType` 64 baytlık serbest bir C dizesidir ve işi gönderen
            // istemciden gelir; argv'deki `title`/`user` kadar güvenilmez,
            // bu yüzden kaçırılmış olarak basılır.
            eprintln!(
                "WARNING: Tanınmayan MediaType {}; @PJL SET PAPERTYPE={} gönderiliyor. \
                 PPD'nin *MediaType anahtarları yazıcının PJL sözlüğünden olmalıdır: {}.",
                quote_untrusted(&header.media_type),
                spl::PJL_PAPERTYPE_DEFAULT,
                spl::PJL_PAPER_TYPES.join(", ")
            );
            spl::PJL_PAPERTYPE_DEFAULT
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
fn band_height_for(header: &PageHeader) -> usize {
    if header.hw_resolution[0] == 300 && header.hw_resolution[1] == 300 {
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
const MAX_PAGES_PER_JOB: u32 = 1_000;

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
const MAX_JOB_RASTER_BYTES: u64 = 8 * 1024 * 1024 * 1024;

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
const MAX_JOB_IMPRESSIONS: u64 = 10_000;

/// Bir işin tükettiği kaynakların toplu muhasebesi.
///
/// Üç sayacın da tek bir yerde durmasının nedeni, birbirlerinin körlüğünü
/// kapatmaları: sayfa sayısı çözünürlük körü, raster hacmi kopya körü, yaprak
/// sayısı ise sayfa boyutu körüdür. Ayrı ayrı uygulandıklarında aralarındaki
/// çarpımsal boşluklar (bkz. `MAX_JOB_IMPRESSIONS`) gözden kaçıyordu.
#[derive(Debug, Default)]
struct JobBudget {
    pages: u32,
    raster_bytes: u64,
    impressions: u64,
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
    fn account_page(&mut self, page_raster_bytes: u64, copies: u16) -> io::Result<u32> {
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
const MAX_REALISTIC_COPIES: u16 = 999;

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
fn sanitize_copies(num_copies: u32) -> u16 {
    num_copies.clamp(1, MAX_REALISTIC_COPIES as u32) as u16
}

/// Standart CUPS Raster akışını okur ve Samsung QPDL/SPL2 formatına dönüştürür.
///
/// `writer` parametre olarak alınır (doğrudan `io::stdout()` kullanılmaz) ki
/// testler üretilen SPL akışını inceleyebilsin; özellikle hata yollarında
/// kapanış UEL'inin yazıldığını doğrulamak için gerekli.
///
/// `service_date` de aynı gerekçeyle DIŞARIDAN veriliyor (doğrudan
/// `current_service_date()` çağrılmıyor): `@PJL DEFAULT SERVICEDATE` satırı
/// bugünün tarihini taşıdığı için üretilen akış saate bağımlı olurdu ve
/// altın dosya (golden file) karşılaştırması her gece yarısı kırılırdı.
/// Tarihi sabitlemek yerine karşılaştırmayı gevşetmek, gerçek sapmaları da
/// gizlerdi; bkz. `src/golden.rs`. `main` yine `current_service_date()`
/// geçiyor, yani çalışma zamanı davranışı değişmiyor.
fn process_cups_raster_to_spl<W: Write>(
    args: &CupsFilterArgs,
    reader: Box<dyn Read>,
    writer: W,
    service_date: &str,
) -> io::Result<()> {
    process_with_margin(args, reader, writer, service_date, media::HARD_MARGIN_PT)
}

// The production path always supplies the driver constant. The explicit input
// also preserves synthetic geometry cases in the golden harness.
fn process_with_margin<W: Write>(
    args: &CupsFilterArgs,
    reader: Box<dyn Read>,
    writer: W,
    service_date: &str,
    margin_pt: f64,
) -> io::Result<()> {
    if !margin_pt.is_finite() || margin_pt <= 0.0 || margin_pt > 36.0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid driver hard margin",
        ));
    }
    // 1. CUPS Raster başlık/magic kontrolü (RaSt, RaS2, RaS3 vb.)
    let mut raster_reader = CupsRasterReader::new(reader)?;

    eprintln!(
        "INFO: Geçerli CUPS Raster akışı tespit edildi (Sürüm: {:?}, Endian: {})",
        raster_reader.version(),
        if raster_reader.version().is_big_endian() {
            "Big Endian"
        } else {
            "Little Endian"
        }
    );

    let mut spl_writer = SplStreamWriter::new(writer);

    // İlk sayfa başlığını, işi başlatmadan (begin_job) önce oku: CUPS Raster
    // duplex bilgisini SAYFA başlığında taşır, ama PJL iş başlığı duplex'i
    // İŞ seviyesinde bildirmek zorunda. Bu yüzden ilk başlığı "peek" edip iş
    // yapılandırmasını buna göre kuruyoruz; döngüde tekrar okumuyoruz.
    let mut next_header = raster_reader.next_page_header()?;

    let job_duplex = match &next_header {
        Some(h) => duplex_mode(h),
        None => SplDuplex::Simplex,
    };

    // Kağıt türü de duplex gibi İŞ seviyesinde (PJL) bildirilir, oysa CUPS
    // onu SAYFA başlığında taşır; bu yüzden aynı "ilk başlığı peek et"
    // kalıbı kullanılıyor. Sayfa başına farklı bir kağıt türü QPDL'de zaten
    // ifade edilemiyor.
    let job_paper_type = match &next_header {
        Some(h) => pjl_paper_type_for(h),
        None => spl::PJL_PAPERTYPE_DEFAULT,
    };

    // 2. Samsung ML-2160 serisi PJL Başlığı (@PJL ENTER LANGUAGE = QPDL)
    let job_config = JobConfig {
        job_name: args
            .title
            .clone()
            .unwrap_or_else(|| "CUPS Document".to_string()),
        user_name: args.user.clone().unwrap_or_else(|| "guest".to_string()),
        service_date: service_date.to_string(),
        duplex: job_duplex,
        paper_type: job_paper_type,
    };
    spl_writer.begin_job(&job_config)?;

    let mut page_number = 0;
    let mut budget = JobBudget::default();

    // 3. Sayfa Döngüsü
    while let Some(header) = next_header.take() {
        validate_page_header(&header)?;

        // Kopya sayısı bir kez normalize edilir ve hem bütçeye hem yazıcıya
        // AYNI değer gider; iki yerde ayrı ayrı hesaplamak, bütçenin fiilen
        // basılmayacak kopyaları saymasına yol açardı.
        let copies = sanitize_copies(header.num_copies);

        // Sayfa sayısı, ham raster hacmi ve yaprak sayısı sınırları; gerekçe
        // için `JobBudget`e bakın. Doğrulamadan SONRA sayılıyor, böylece
        // reddedilen bir sayfa bütçeyi tüketmez.
        page_number = budget.account_page(header.total_raster_bytes(), copies)?;

        // `sanitize_copies` ile aynı değer loglanır: aksi halde bu satır,
        // yazıcıya fiilen gönderilen (aşağıda begin_page/end_page ile
        // sanitize_copies() üzerinden yazılan) kopya sayısından farklı,
        // ham/sınırsız `header.num_copies` değerini gösterip yanıltıcı
        // teşhis bilgisi üretebilirdi (ör. 65536 istenirse burada "65536"
        // yazılır ama yazıcıya 999 gönderilirdi).
        eprintln!("PAGE: {} {}", page_number, copies);
        eprintln!("INFO: Sayfa {} başlatılıyor...", page_number);

        print_header_info(page_number, &header);

        // `cupsCompression`, CUPS Raster'da AKIŞ sıkıştırması değil, sürücüye
        // özel bir "cihaz sıkıştırması" ipucudur (akış sıkıştırması sync
        // sözcüğüyle belirlenir; bkz. raster.rs is_compressed). SpliX bu alanı
        // kullanmaz, bu filtre de bant sıkıştırmasını her zaman Algo 0x11 ile
        // yapar; dolayısıyla alan bilinçli olarak yok sayılıyor. Sıfırdan
        // farklıysa, PPD ile bu filtrenin varsayımları arasında bir uyuşmazlık
        // olabileceği için teşhis amaçlı bir kez bildiriyoruz.
        if header.compression != 0 {
            eprintln!(
                "WARNING: cupsCompression={} yok sayıldı; bant sıkıştırması her zaman Algo 0x11 RLE.",
                header.compression
            );
        }

        // SpliX pageWidth hesabı: fiziksel sayfa genişliğini DPI ile piksele çevir, 8'e hizala
        // SpliX document.cpp: pageWidth = ((ceil(pageSizePt * dpi / 72) + 7) & ~7)
        let page_width_pixels =
            compute_page_width_pixels(header.page_size_points[0], header.hw_resolution[0]);

        // SpliX compress.cpp (M2026 öncesi orijinal mantık):
        //   bandWidthInB = lineWidthInB = (pageWidth + 7) / 8
        //   bandWidth = bandWidthInB * 8
        // ML-2160 serisi için 256-hizalama kullanılmıyor.
        let band_width_bytes = (page_width_pixels + 7) / 8;
        let band_width_pixels = band_width_bytes * 8;

        // QPDL sayfa/bant kayıtlarındaki genişlik ve yükseklik alanları
        // 16-bit'tir. Değerleri `as u16` ile sessizce kırpmak, yazıcıya
        // bildirilen boyut ile gerçek payload'ın uyuşmamasına (DMA/RLE çözme
        // senkron kaybı) yol açar; bunun yerine `try_into` ile erken ve net
        // bir hata döndürüyoruz. `PageConfig` alanları da `u16` olduğu için
        // dönüşümü atlamak mümkün değil (bkz. spl.rs PageConfig).
        //
        // Her ikisi de pratikte `validate_page_header`'ın sınırları sayesinde
        // zaten sığıyor; bu dönüşümler o dolaylı korumayı doğrudan ve
        // sınırlar değişse bile geçerli kalan bir garantiye çeviriyor.
        let to_u16 = |value: u32, alan: &str| -> io::Result<u16> {
            u16::try_from(value).map_err(|_| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!("{} QPDL'nin 16-bit alanına sığmıyor: {} px", alan, value),
                )
            })
        };
        let band_width_u16 = to_u16(band_width_pixels, "Bant genişliği")?;
        let page_height_u16 = to_u16(header.height, "Sayfa yüksekliği")?;

        // CUPS raster verisi (595B) bant genişliğinde (620B) ortalanır, sonra
        // yazıcının sert kenar boşluğu düşülür; bkz. `band_placement`.
        let cups_line_bytes = header.bytes_per_line as usize;
        let hard_margin = hard_margin_bytes(margin_pt, header.hw_resolution[0]);
        let placement = band_placement(band_width_bytes as usize, cups_line_bytes, hard_margin)?;
        if (band_width_bytes as usize) < cups_line_bytes {
            // Uyarı yerleşimden SONRA basılıyor, çünkü hangi kenarın kırpıldığı
            // `src_skip`e bağlı: bant satırdan darken sert kenar boşluğu da
            // varsa satırın solundan bayt atılır, yani yalnızca sağ kenar
            // kırpılmaz. Operatörü yanlış kenara yönlendirmemek için ikisi de
            // söyleniyor.
            let left_note = if placement.src_skip > 0 {
                format!(
                    " ve sert kenar boşluğu nedeniyle sol kenarından {} B atılacak",
                    placement.src_skip
                )
            } else {
                String::new()
            };
            eprintln!(
                "WARNING: Hesaplanan bant genişliği ({} B) CUPS satır genişliğinden ({} B) dar; \
                 satırların sağ kenarı kırpılacak{}.",
                band_width_bytes, cups_line_bytes, left_note
            );
        }

        eprintln!(
            "DEBUG: QPDL Genişlik: cupsWidth={}, pageWidthPx={}, bandWidthPx={}, bandWidthB={}, \
             hardMarginB={}, dstOffsetB={}, srcSkipB={}",
            header.width,
            page_width_pixels,
            band_width_pixels,
            band_width_bytes,
            hard_margin,
            placement.dst_offset,
            placement.src_skip
        );

        // `validate_page_header` bilinmeyen ölçüleri reddeder. Buradaki ikinci
        // kontrol, bu dönüşüm ileride doğrulamadan ayrı bir çağrı yoluna taşınsa
        // bile A4'e sessiz bir geri düşüşün yeniden oluşmasını engeller.
        let paper_size = SplPaperSize::from_dimensions_pt_exact(
            header.page_size_points[0],
            header.page_size_points[1],
        )
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "Doğrulanmış kâğıt ölçüsü QPDL koduna dönüştürülemedi",
            )
        })?;

        // Kağıt kaynağı, PPD'nin `*InputSlot` seçeneğinden CUPS Raster
        // başlığının `MediaPosition` alanı üzerinden geliyor (bkz. spl.rs
        // `SplPaperSource::from_media_position`). Bu alan daha önce hiç
        // okunmuyor ve kaynak koşulsuz `Auto` gönderiliyordu; yani PPD'nin
        // sunduğu "Manual Feeder" seçeneği kullanıcı için hiçbir şey
        // yapmıyordu. Ayrım argv'deki `options` dizesinden değil başlıktan
        // okunuyor, çünkü bağlayıcı olan başlıktır (bkz. dosya başındaki
        // `CupsFilterArgs` notu).
        let paper_source = match SplPaperSource::from_media_position(header.media_position) {
            Some(source) => source,
            None => {
                eprintln!(
                    "WARNING: Tanınmayan MediaPosition değeri: {}; QPDL kağıt kaynağı \
                     Auto olarak gönderiliyor. PPD'nin *InputSlot seçenekleri QPDL \
                     kodlarıyla numaralandırılmalıdır (1=Auto, 2=Manual, 3=Multi, \
                     4=Upper, 5=Lower).",
                    header.media_position
                );
                SplPaperSource::Auto
            }
        };

        let resolution_x =
            SplResolution::from_dpi_exact(header.hw_resolution[0]).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Yatay çözünürlük QPDL koduna dönüştürülemedi",
                )
            })?;
        let resolution_y =
            SplResolution::from_dpi_exact(header.hw_resolution[1]).ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Dikey çözünürlük QPDL koduna dönüştürülemedi",
                )
            })?;

        let page_config = PageConfig {
            paper_size,
            // ELLE DUPLEX EKSİĞİ (1/2): SpliX qpdl.cpp renderPage, elle duplex
            // modunda ön yüz geçişinde kağıt kaynağını GEÇİCİ olarak değiştirir:
            //
            //   if (tumble && !lastPage) paperSource = 3; // Multi source
            //
            // Buradaki `lastPage` bilgisi AKIŞ HALİNDE üretilemiyor: CUPS
            // Raster'da sayfa verisi tüketilmeden bir sonraki sayfa başlığı
            // okunamaz, dolayısıyla sayfa başlığını yazarken bu sayfanın son
            // sayfa olup olmadığı bilinmiyor. Doğru uygulamak sayfanın tüm
            // bantlarını bellekte tamponlamayı gerektirir (en kötü durumda ~22
            // MB), ki bu yol bugün zaten erişilemez (bkz. duplex_mode).
            // Bu sapma yalnızca ELLE DUPLEX'i etkiler; tek taraflı işlerde
            // kaynak aşağıdaki gibi kullanıcının seçtiği tepsidir.
            paper_source,
            // Eksenler AYRI: QPDL `header[0x1]` dikey, `header[0x10]` yatay
            // çözünürlüğü taşır (bkz. spl.rs PageConfig). `1200x600dpi` bu
            // motor ailesinde gerçek bir moddur.
            resolution_x,
            resolution_y,
            duplex: duplex_mode(&header),
            // `tumble` baytı sayfa numarasının paritesinden üretiliyor ve
            // sayaç 1 tabanlı; `page_number` da öyle (yukarıda kullanımdan
            // ÖNCE artırılıyor). Bkz. spl.rs PageConfig::page_number.
            page_number,
            copies,
            // SpliX qpdl.cpp renderPage: width = page->width() = pageWidth
            width_pixels: band_width_u16,
            height_pixels: page_height_u16,
            qpdl_version: 3,
        };

        // 17-Baytlık QPDL Sayfa Başlığı
        spl_writer.begin_page(&page_config)?;

        // Sayfa şeritlerini SpliX uyumlu stride ile aktar
        stream_page_bands(
            &mut raster_reader,
            &mut spl_writer,
            &header,
            band_width_u16,
            band_width_bytes,
            placement,
        )?;

        // 3-Baytlık QPDL Sayfa Sonu
        spl_writer.end_page(copies)?;

        eprintln!("INFO: Sayfa {} tamamlandı.\n", page_number);

        next_header = raster_reader.next_page_header()?;
    }

    if page_number == 0 {
        eprintln!("WARNING: CUPS Raster akışında sayfa bulunamadı.");
    } else {
        eprintln!(
            "INFO: Toplam {} sayfa başarıyla SPL/QPDL formatına dönüştürüldü.",
            page_number
        );
    }

    // İş Sonu (PJL UEL). Sayfa bulunamamış olsa bile çağrılır: `begin_job`
    // yazıcıyı çoktan QPDL diline soktuğu için akış her hâlükârda kapanış
    // UEL'i ile bitmelidir. `end_job` içeride flush eder.
    spl_writer.end_job()?;
    Ok(())
}

/// Raster verisini SpliX uyumlu şeritler (bands) halinde yazar.
///
/// SpliX document.cpp + compress.cpp akışını takip eder:
/// 1. CUPS raster satırını okur (cupsBytesPerLine bayt)
/// 2. bandWidthInB genişliğinde bir şerit tampona, `BandPlacement`e göre kopyalar
/// 3. Şeridi Algo 0x11 RLE ile sıkıştırır
/// 4. QPDL Record 0x0C + Subheader 0x09ABCDEF + Payload + Checksum olarak yazar
fn stream_page_bands<R: Read, W: Write>(
    raster_reader: &mut CupsRasterReader<R>,
    spl_writer: &mut SplStreamWriter<W>,
    header: &PageHeader,
    band_width_pixels: u16,
    band_width_bytes: u32,
    placement: BandPlacement,
) -> io::Result<()> {
    let cups_bytes_per_line = header.bytes_per_line as usize;
    let total_lines = header.height as usize;
    let bw_bytes = band_width_bytes as usize;

    let band_height = band_height_for(header);

    // CUPS raster satır okuma tamponu
    let mut line_buffer = vec![0u8; cups_bytes_per_line];

    // Kopyalanacak bayt sayısı: satırın atlanan solundan sonra kalan kısmı,
    // bant tamponunda içeriğe ayrılan yere sığmalı.
    let bytes_to_copy =
        (cups_bytes_per_line - placement.src_skip).min(bw_bytes - placement.dst_offset);

    // Bant tamponu (bandWidthInB x bandHeight bayt) bir kez ayrılır ve her
    // bandın başında sıfırlanır: son bant eksik satırlı olabileceğinden
    // sıfırlama zorunlu, ama sayfa başına onlarca kez yeniden tahsis etmek
    // gereksiz.
    let mut band_data = vec![0u8; bw_bytes * band_height];

    let mut current_line = 0;

    while current_line < total_lines {
        let lines_in_this_band = (total_lines - current_line).min(band_height);
        band_data.fill(0);

        for y in 0..lines_in_this_band {
            raster_reader.read_line(&mut line_buffer)?;

            // SpliX algo0x11.h: Algo0x11::reverseLineColumn() == true, yani
            // compress.cpp'deki _compressBandedPage bant tamponunu SÜTUN-ÖNCELİKLİ
            // (transpoze) doldurur:
            //   band[x * bandHeight + y] = planes[i][x + hardMarginXInB + ...]
            // Satır-öncelikli (row-major) doldurma, sıkıştırma kendisi doğru
            // çalışsa bile yazıcının transpoze edilmiş/gürültülü bir görüntü
            // çözmesine yol açar.
            for (c, &byte) in line_buffer[placement.src_skip..]
                .iter()
                .take(bytes_to_copy)
                .enumerate()
            {
                let col = placement.dst_offset + c;
                band_data[col * band_height + y] = byte;
            }
        }

        // Samsung ML-2160 serisi QPDL lazer motoru, CUPS K renk uzayının TERSİ polariteyle çalışır:
        //   CUPS K:     0 = beyaz (toner yok),  1 = siyah (toner var)
        //   Samsung:    0 = siyah (toner bas),   1 = beyaz (toner yok)
        // Empirik test: tersleme olmadan sayfa simsiyah çıkıyor.
        for b in &mut band_data {
            *b = !*b;
        }

        // QPDL Şerit Kaydı (SpliX uyumlu)
        spl_writer.write_compressed_band(band_width_pixels, band_height as u16, &band_data)?;

        current_line += lines_in_this_band;
    }

    Ok(())
}

/// CUPS Raster sayfa başlığından elde edilen meta verileri formatlayıp stderr'e basar.
fn print_header_info(page_num: u32, header: &PageHeader) {
    // Her satır `DEBUG: ` ile başlıyor. CUPS, bir filtrenin stderr'inde
    // tanıdığı önekleri (DEBUG/INFO/WARNING/ERROR/PAGE/...) o seviyeye
    // yönlendirir; ÖNEKSİZ satırları da DEBUG sayar, yani varsayılan
    // `LogLevel warn` altında davranış aynıdır. Fark `LogLevel debug`ta ortaya
    // çıkıyordu: bu blok sayfa başına ~15 satır üretiyor ve öneksiz satırlar
    // niyeti belirsiz bırakıyordu. Önek, satırların teşhis amaçlı olduğunu
    // hem CUPS'a hem logu okuyana açıkça söyler.
    eprintln!("DEBUG: --------------------------------------------------");
    eprintln!("DEBUG:  [CUPS RASTER SAYFA {} META VERİLERİ]", page_num);
    eprintln!(
        "DEBUG:   Çözünürlük (DPI): {} x {}",
        header.hw_resolution[0], header.hw_resolution[1]
    );
    eprintln!(
        "DEBUG:   Boyutlar (px)   : {} x {} (Genişlik x Yükseklik)",
        header.width, header.height
    );
    eprintln!(
        "DEBUG:   Sayfa Boyutu(pt): {} x {} pt",
        header.page_size_points[0], header.page_size_points[1]
    );
    if let Some(name) = &header.page_size_name {
        // `cupsPageSizeName` raster başlığındaki 64 baytlık bir C dizesidir ve
        // işi gönderen istemciden gelir — argv'deki `title`/`user` kadar
        // güvenilmezdir, bu yüzden ham değil kaçırılmış olarak basılır.
        eprintln!("DEBUG:   Medya Adı       : {}", quote_untrusted(name));
    }
    eprintln!("DEBUG:   Renk Uzayı      : {}", header.color_space);
    eprintln!("DEBUG:   Renk Dizilimi   : {:?}", header.color_order);
    eprintln!("DEBUG:   Kanal Bit Derin.: {}", header.bits_per_color);
    eprintln!("DEBUG:   Piksel Bit Der. : {}", header.bits_per_pixel);
    eprintln!("DEBUG:   Satır Bayt Say. : {} bayt", header.bytes_per_line);
    eprintln!(
        "DEBUG:   Ham Raster Boy. : {} bayt ({:.2} MB)",
        header.total_raster_bytes(),
        header.total_raster_bytes() as f64 / (1024.0 * 1024.0)
    );
    eprintln!(
        "DEBUG:   Çift Taraflı    : {}",
        if header.duplex { "Açık" } else { "Kapalı" }
    );
    eprintln!("DEBUG:   Kopya Sayısı    : {}", header.num_copies);
    eprintln!(
        "DEBUG:   Kağıt Kaynağı   : MediaPosition={} -> {:?}",
        header.media_position,
        SplPaperSource::from_media_position(header.media_position)
    );
    // Uyarıyı `pjl_paper_type_for` iş başlığı kurulurken bir kez basıyor;
    // burada yalnızca eşlemenin sonucu gösteriliyor (sayfa başına tekrar
    // eden bir uyarı üretmemek için).
    eprintln!(
        "DEBUG:   Kağıt Türü      : MediaType={} -> PAPERTYPE={}",
        quote_untrusted(&header.media_type),
        spl::pjl_paper_type(&header.media_type).unwrap_or(spl::PJL_PAPERTYPE_DEFAULT)
    );
    eprintln!("DEBUG: --------------------------------------------------");
}

#[cfg(test)]
mod tests {
    use super::*;
    use raster::CupsRasterVersion;
    use std::io::Cursor;

    /// Argümansız (doğrudan boru hattı) çağrıyı temsil eder.
    fn no_args() -> CupsFilterArgs {
        CupsFilterArgs::default()
    }

    /// Projenin PPD dosyasını okur. Aşağıdaki testlerin bir bölümü, filtrenin
    /// sabitlerini PPD'nin gerçek içeriğine bağlar: PPD'ye yeni bir seçenek
    /// eklendiğinde sabitleri güncellemeyi unutmak testi kırar.
    fn ppd_text() -> String {
        std::fs::read_to_string(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/ppd/samsung-ml2160.ppd"
        ))
        .expect("PPD okunamadı")
    }

    /// PPD'nin sunduğu `*Resolution` seçenekleri, (x_dpi, y_dpi) olarak.
    /// `600dpi` gibi tek değerli adlar iki eksene de yazılır.
    fn ppd_resolutions() -> Vec<(u32, u32)> {
        let ppd = ppd_text();
        let list: Vec<(u32, u32)> = ppd
            .lines()
            .filter_map(|line| {
                let rest = line.strip_prefix("*Resolution ")?;
                let name = rest.split('/').next().unwrap_or("").trim_end_matches("dpi");
                let parse = |v: &str| {
                    v.parse::<u32>()
                        .unwrap_or_else(|_| panic!("PPD çözünürlüğü ayrıştırılamadı: {}", line))
                };
                Some(match name.split_once('x') {
                    Some((x, y)) => (parse(x), parse(y)),
                    None => (parse(name), parse(name)),
                })
            })
            .collect();
        assert!(
            list.len() >= 4,
            "PPD'den çözünürlük okunamadı: {}",
            list.len()
        );
        list
    }

    /// PPD'nin sunduğu `*PaperDimension` seçenekleri, (ad, genişlik_pt,
    /// yükseklik_pt) olarak. Ondalık yazılmış ölçüler tavana yuvarlanır.
    fn ppd_paper_dimensions() -> Vec<(String, u32, u32)> {
        let ppd = ppd_text();
        let list: Vec<(String, u32, u32)> = ppd
            .lines()
            .filter_map(|line| {
                let rest = line.strip_prefix("*PaperDimension ")?;
                let (name, dims) = rest.split_once(':').expect("bozuk *PaperDimension satırı");
                let name = name.split('/').next().unwrap().trim().to_string();
                let mut it = dims.trim().trim_matches('"').split_whitespace();
                let mut pt = || {
                    it.next()
                        .and_then(|v| v.parse::<f64>().ok())
                        .unwrap_or_else(|| panic!("PPD kağıt boyutu ayrıştırılamadı: {}", line))
                        .ceil() as u32
                };
                let (w, h) = (pt(), pt());
                Some((name, w, h))
            })
            .collect();
        assert!(
            list.len() >= 10,
            "PPD'den kağıt boyutu okunamadı: {}",
            list.len()
        );
        list
    }

    #[test]
    fn test_sanitize_copies_never_returns_zero() {
        assert_eq!(sanitize_copies(0), 1);
        assert_eq!(sanitize_copies(1), 1);
        assert_eq!(sanitize_copies(5), 5);
        assert_eq!(
            sanitize_copies(MAX_REALISTIC_COPIES as u32),
            MAX_REALISTIC_COPIES
        );
        // Eski davranış: `.max(1) as u16` burada 0 döndürüyordu.
        assert_eq!(sanitize_copies(65536), MAX_REALISTIC_COPIES);
        assert_eq!(sanitize_copies(131072), MAX_REALISTIC_COPIES);
        assert_eq!(sanitize_copies(u32::MAX), MAX_REALISTIC_COPIES);
    }

    /// A4, 600 DPI, 1-bit monokrom (K), tutarlı `bytesPerLine`'a sahip
    /// geçerli bir başlık üretir; testler bunu temel alıp tek bir alanı
    /// bozarak `validate_page_header`'ın onu reddettiğini doğrular.
    fn valid_header() -> PageHeader {
        let mut buf = vec![0u8; 1796];
        buf[276..280].copy_from_slice(&600u32.to_be_bytes()); // hw_resolution[0]
        buf[280..284].copy_from_slice(&600u32.to_be_bytes()); // hw_resolution[1]
        buf[352..356].copy_from_slice(&595u32.to_be_bytes()); // page_size_points[0] (A4)
        buf[356..360].copy_from_slice(&842u32.to_be_bytes()); // page_size_points[1]
        buf[372..376].copy_from_slice(&8u32.to_be_bytes()); // width
        buf[376..380].copy_from_slice(&8u32.to_be_bytes()); // height
        buf[384..388].copy_from_slice(&1u32.to_be_bytes()); // bits_per_color
        buf[388..392].copy_from_slice(&1u32.to_be_bytes()); // bits_per_pixel
        buf[392..396].copy_from_slice(&1u32.to_be_bytes()); // bytes_per_line = ceil(8*1/8)
        buf[400..404].copy_from_slice(&3u32.to_be_bytes()); // color_space = K
        PageHeader::parse(&buf, CupsRasterVersion::V2Be).unwrap()
    }

    #[test]
    fn test_validate_page_header_accepts_valid_mono_header() {
        assert!(validate_page_header(&valid_header()).is_ok());
    }

    #[test]
    fn test_validate_page_header_rejects_unsupported_resolution_without_rounding() {
        for resolution in [[599, 599], [600, 1200], [300, 600], [1200, 300]] {
            let mut header = valid_header();
            header.hw_resolution = resolution;
            let err = validate_page_header(&header)
                .expect_err("PPD dışındaki çözünürlük reddedilmeliydi");
            assert!(
                err.to_string().contains("Desteklenmeyen çözünürlük"),
                "{}x{} için yanlış hata: {}",
                resolution[0],
                resolution[1],
                err
            );
        }
    }

    #[test]
    fn test_validate_page_header_rejects_unknown_paper_geometry() {
        let mut header = valid_header();
        header.page_size_points = [612, 936];
        let err = validate_page_header(&header)
            .expect_err("QPDL kodu olmayan kâğıt ölçüsü reddedilmeliydi");
        assert!(
            err.to_string().contains("Desteklenmeyen kâğıt ölçüsü"),
            "{}",
            err
        );
    }

    #[test]
    fn test_validate_page_header_rejects_non_k_color_space() {
        let mut header = valid_header();
        header.color_space = CupsColorSpace(1); // RGB
        assert!(validate_page_header(&header).is_err());
    }

    #[test]
    fn test_validate_page_header_rejects_wrong_bit_depth() {
        // 24-bit RGB / 32-bit CMYK gibi çok bitli bir akış simüle edilir.
        let mut header = valid_header();
        header.bits_per_color = 8;
        header.bits_per_pixel = 24;
        assert!(validate_page_header(&header).is_err());
    }

    #[test]
    fn test_validate_page_header_rejects_inconsistent_bytes_per_line() {
        let mut header = valid_header();
        header.bytes_per_line = 999; // width=8, bits_per_pixel=1 ile uyuşmuyor
        assert!(validate_page_header(&header).is_err());
    }

    /// Geçerli, tek sayfalık, sıkıştırmasız bir V3 Big-Endian raster akışı
    /// üretir. `pixel_bytes`, sayfa verisinin kaç baytının yazılacağını
    /// belirler; başlıkta bildirilenden az vermek kısa okuma (hata) yolunu
    /// tetikler.
    fn v3_stream(pixel_bytes: usize) -> Vec<u8> {
        let mut buf = vec![0u8; 1796];
        let mut put = |off: usize, val: u32| {
            buf[off..off + 4].copy_from_slice(&val.to_be_bytes());
        };
        put(276, 600); // hw_resolution[0]
        put(280, 600); // hw_resolution[1]
        put(352, 595); // page_size_points[0] (A4)
        put(356, 842); // page_size_points[1]
        put(372, 32); // width
        put(376, 4); // height
        put(384, 1); // bits_per_color
        put(388, 1); // bits_per_pixel
        put(392, 4); // bytes_per_line = ceil(32 * 1 / 8)
        put(400, 3); // color_space = K

        let mut stream = b"RaS3".to_vec();
        stream.extend_from_slice(&buf);
        stream.extend_from_slice(&vec![0u8; pixel_bytes]);
        stream
    }

    /// İstenen geometri/duplex ile çok sayfalı, sıkıştırmasız (v3) bir CUPS
    /// Raster akışı kurar. `v3_stream` sabit A4/600 DPI üretiyor; bu esnek
    /// sürüm çözünürlük ve duplex bayraklarını değiştirebilmek için var.
    struct RasterSpec {
        res_x: u32,
        res_y: u32,
        page_pt: (u32, u32),
        width_px: u32,
        height: u32,
        duplex: bool,
        tumble: bool,
        pages: usize,
        /// CUPS `MediaPosition` (PPD `*InputSlot`).
        media_position: u32,
        /// CUPS `MediaType` (PPD `*MediaType`); boş = seçilmemiş.
        media_type: &'static str,
        /// CUPS `Margins[0]` (PPD `*ImageableArea`nın sol kenar boşluğu, pt).
        margin_left_pt: u32,
        /// Her raster satırı için kullanılacak desen; `None` = tamamen boş
        /// satır. Uzunluğu `bytes_per_line()` olmalıdır.
        line_pattern: Option<Vec<u8>>,
    }

    impl RasterSpec {
        /// A4, verilen çözünürlükte sayfa genişliğine tam oturan bir sayfa.
        fn a4(res_x: u32, res_y: u32, height: u32) -> Self {
            let width_px = compute_page_width_pixels(595, res_x);
            Self {
                res_x,
                res_y,
                page_pt: (595, 842),
                width_px,
                height,
                duplex: false,
                tumble: false,
                pages: 1,
                media_position: 0,
                media_type: "",
                margin_left_pt: 0,
                line_pattern: None,
            }
        }

        fn bytes_per_line(&self) -> u32 {
            self.width_px.div_ceil(8)
        }

        fn build(&self) -> Vec<u8> {
            let mut stream = b"RaS3".to_vec();
            for _ in 0..self.pages {
                let mut buf = vec![0u8; 1796];
                let mut put = |off: usize, val: u32| {
                    buf[off..off + 4].copy_from_slice(&val.to_be_bytes());
                };
                put(272, self.duplex as u32);
                put(312, self.margin_left_pt); // Margins[0] (sol, pt)
                put(324, self.media_position);
                put(276, self.res_x);
                put(280, self.res_y);
                put(352, self.page_pt.0);
                put(356, self.page_pt.1);
                put(368, self.tumble as u32); // Tumble (cupsWidth'ten hemen önce)
                put(372, self.width_px);
                put(376, self.height);
                put(384, 1); // bits_per_color
                put(388, 1); // bits_per_pixel
                put(392, self.bytes_per_line());
                put(400, 3); // color_space = K
                let media_type = self.media_type.as_bytes();
                buf[128..128 + media_type.len()].copy_from_slice(media_type);
                stream.extend_from_slice(&buf);
                let line = match &self.line_pattern {
                    Some(pattern) => {
                        assert_eq!(
                            pattern.len(),
                            self.bytes_per_line() as usize,
                            "desen satır uzunluğuyla uyuşmuyor"
                        );
                        pattern.clone()
                    }
                    None => vec![0u8; self.bytes_per_line() as usize],
                };
                for _ in 0..self.height {
                    stream.extend_from_slice(&line);
                }
            }
            stream
        }
    }

    /// Üretilen SPL akışındaki İLK şerit kaydının payload'ını çözüp,
    /// `stream_page_bands`'in yazdığı bant tamponunu (terslemeden ÖNCEKİ
    /// hâliyle) geri verir.
    ///
    /// Tampon transpozedir: `band[col * band_height + y]`.
    fn first_band_buffer(out: &[u8]) -> Vec<u8> {
        // Kayıt konumu deterministik olarak bulunur: 0x0C baytını aramak
        // güvenli değil, çünkü sayfa başlığının 0x4 baytı da (EnvIsoB5 kağıt
        // kodu) 0x0C olabilir.
        const QPDL_MARK: &[u8] = b"ENTER LANGUAGE = QPDL\n";
        let pos = out
            .windows(QPDL_MARK.len())
            .position(|w| w == QPDL_MARK)
            .expect("QPDL diline geçiş satırı yok")
            + QPDL_MARK.len()
            + 17; // 17 baytlık sayfa başlığından sonrası
        assert_eq!(out[pos], 0x0C, "şerit kaydı imzası beklendi");
        assert_eq!(out[pos + 6], 0x11, "Algo 0x11 bekleniyordu");
        let total = u32::from_be_bytes(out[pos + 7..pos + 11].try_into().unwrap()) as usize;
        // 11 bayt kayıt başlığı + 4 bayt alt başlık; sondaki 4 bayt checksum.
        let payload = &out[pos + 15..pos + 11 + total - 4];
        let mut band = spl::Algo0x11::decompress(payload);
        // `stream_page_bands` yazmadan hemen önce tersliyor; geri al.
        for b in &mut band {
            *b = !*b;
        }
        band
    }

    /// Transpoze bant tamponunda, ilk satırdaki (`y == 0`) sıfır olmayan
    /// baytların sütun indislerini döner.
    fn nonzero_columns_in_first_line(band: &[u8], band_height: usize) -> Vec<usize> {
        band.chunks(band_height)
            .enumerate()
            .filter(|(_, col)| col[0] != 0)
            .map(|(idx, _)| idx)
            .collect()
    }

    /// Üretilen SPL akışındaki tek bir şerit kaydının başlık alanları.
    #[derive(Debug, PartialEq, Eq)]
    struct BandRecord {
        index: u8,
        width_px: u16,
        height_lines: u16,
    }

    /// Üretilen SPL akışındaki tek bir sayfa: 17 baytlık başlık + şeritleri.
    #[derive(Debug)]
    struct SplPage {
        header: [u8; 17],
        bands: Vec<BandRecord>,
    }

    /// SPL çıktısını gerçek kayıt yapısına göre ayrıştırır. Testlerin
    /// varsayımlarını değil, tele yazılan baytları doğrulayabilmesi için.
    fn parse_spl(out: &[u8]) -> Vec<SplPage> {
        const QPDL_MARK: &[u8] = b"ENTER LANGUAGE = QPDL\n";
        let start = out
            .windows(QPDL_MARK.len())
            .position(|w| w == QPDL_MARK)
            .expect("QPDL diline geçiş satırı yok")
            + QPDL_MARK.len();

        let mut pages = Vec::new();
        let mut pos = start;
        while pos < out.len() && !out[pos..].starts_with(spl::PJL_END) {
            assert_eq!(out[pos], 0x00, "sayfa başlığı imzası beklendi @ {}", pos);
            let mut header = [0u8; 17];
            header.copy_from_slice(&out[pos..pos + 17]);
            pos += 17;

            let mut bands = Vec::new();
            while pos < out.len() && out[pos] == 0x0C {
                let total = u32::from_be_bytes(out[pos + 7..pos + 11].try_into().unwrap()) as usize;
                bands.push(BandRecord {
                    index: out[pos + 1],
                    width_px: u16::from_be_bytes(out[pos + 2..pos + 4].try_into().unwrap()),
                    height_lines: u16::from_be_bytes(out[pos + 4..pos + 6].try_into().unwrap()),
                });
                // 11 baytlık kayıt başlığı + (alt başlık + payload + checksum)
                pos += 11 + total;
            }

            assert_eq!(out[pos], 0x01, "sayfa sonu imzası beklendi @ {}", pos);
            pos += 3;
            pages.push(SplPage { header, bands });
        }
        pages
    }

    fn run_filter(stream: Vec<u8>) -> Vec<u8> {
        let mut out: Vec<u8> = Vec::new();
        process_cups_raster_to_spl(
            &no_args(),
            Box::new(Cursor::new(stream)),
            &mut out,
            &current_service_date(),
        )
        .expect("filtre geçerli akışı işleyemedi");
        out
    }

    fn count_uel(stream: &[u8]) -> usize {
        stream
            .windows(spl::PJL_UEL.len())
            .filter(|w| *w == spl::PJL_UEL)
            .count()
    }

    /// Y-03 regresyonu (uçtan uca): sayfa verisi yarıda kesilirse dönüşüm
    /// hata döndürmeli, AMA yazıcıya giden akış yine de kapanış UEL'i ile
    /// bitmeli. Aksi hâlde yazıcı QPDL dilinde, yarım bir bant kaydını
    /// bekler hâlde asılı kalır.
    #[test]
    fn test_closing_uel_written_on_truncated_page_data() {
        let stream = v3_stream(2); // 4 satır x 4 bayt = 16 bayt gerekiyordu
        let mut out: Vec<u8> = Vec::new();
        let err = process_cups_raster_to_spl(
            &no_args(),
            Box::new(Cursor::new(stream)),
            &mut out,
            &current_service_date(),
        )
        .expect_err("kısa sayfa verisi hata döndürmeliydi");
        assert_eq!(err.kind(), io::ErrorKind::UnexpectedEof);
        assert!(
            out.ends_with(spl::PJL_END),
            "hata yolu akışı kapanış UEL'i olmadan bıraktı"
        );
        assert_eq!(count_uel(&out), 2);
    }

    /// Y-03 regresyonu: sayfa başlığı doğrulamadan geçemezse de aynı garanti.
    #[test]
    fn test_closing_uel_written_on_invalid_page_header() {
        let mut stream = v3_stream(16);
        // bytes_per_line'ı 0 yap: validate_page_header reddedecek.
        stream[4 + 392..4 + 396].copy_from_slice(&0u32.to_be_bytes());

        let mut out: Vec<u8> = Vec::new();
        let err = process_cups_raster_to_spl(
            &no_args(),
            Box::new(Cursor::new(stream)),
            &mut out,
            &current_service_date(),
        )
        .expect_err("geçersiz başlık hata döndürmeliydi");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(out.ends_with(spl::PJL_END));
    }

    /// Akışta hiç sayfa yoksa da iş kapatılmalı: `begin_job` yazıcıyı çoktan
    /// QPDL diline sokmuştur.
    #[test]
    fn test_closing_uel_written_when_stream_has_no_pages() {
        let mut out: Vec<u8> = Vec::new();
        process_cups_raster_to_spl(
            &no_args(),
            Box::new(Cursor::new(b"RaS3".to_vec())),
            &mut out,
            &current_service_date(),
        )
        .expect("sayfasız akış hata değil, uyarı üretmeli");
        assert!(out.ends_with(spl::PJL_END));
        assert_eq!(count_uel(&out), 2);
    }

    /// Başarılı akış tam olarak iki UEL içermeli: `Drop`, açıkça kapatılmış
    /// bir işe üçüncü bir UEL eklememelidir.
    #[test]
    fn test_successful_stream_has_exactly_one_uel_pair() {
        let mut out: Vec<u8> = Vec::new();
        process_cups_raster_to_spl(
            &no_args(),
            Box::new(Cursor::new(v3_stream(16))),
            &mut out,
            &current_service_date(),
        )
        .expect("geçerli akış başarılı olmalı");
        assert!(out.starts_with(spl::PJL_UEL));
        assert!(out.ends_with(spl::PJL_END));
        assert_eq!(count_uel(&out), 2, "fazladan UEL yazıldı");
    }

    /// Y-01 regresyonu (uçtan uca): sıkıştırılmış v2 akışı, aynı içeriğin
    /// sıkıştırmasız v3 hâliyle BİRE BİR aynı SPL çıktısını üretmeli.
    ///
    /// Eskiden v2 sayfa verisi ham piksel sanılıyordu ve yazıcıya çöp bir
    /// sayfa gidiyordu. Bu test, `CupsLineDecoder`'ın araya girdiğini ve
    /// akışın sürümünün çıktıyı hiç etkilemediğini sabitler.
    #[test]
    fn test_v2_and_v3_streams_produce_identical_output() {
        // 4 bayt/satır, 3 satır: 0xAA 0xAA 0xAA 0x55 / 0x00 x4 / 0xFF x4
        let pixels: [&[u8]; 3] = [
            &[0xAA, 0xAA, 0xAA, 0x55],
            &[0x00, 0x00, 0x00, 0x00],
            &[0xFF, 0xFF, 0xFF, 0xFF],
        ];

        let mut hdr = vec![0u8; 1796];
        {
            let mut put = |off: usize, val: u32| {
                hdr[off..off + 4].copy_from_slice(&val.to_be_bytes());
            };
            put(276, 600);
            put(280, 600);
            put(352, 595);
            put(356, 842);
            put(372, 32);
            put(376, 3);
            put(384, 1);
            put(388, 1);
            put(392, 4);
            put(400, 3);
        }

        let mut v3 = b"RaS3".to_vec();
        v3.extend_from_slice(&hdr);
        for line in pixels {
            v3.extend_from_slice(line);
        }

        // Aynı içeriğin CUPS satır-RLE karşılığı.
        let mut v2 = b"RaS2".to_vec();
        v2.extend_from_slice(&hdr);
        v2.extend_from_slice(&[0x00, 0x02, 0xAA, 0x00, 0x55]); // 0xAA x3, 0x55 x1
        v2.extend_from_slice(&[0x00, 0x80]); // satır sonuna kadar boş (K => 0x00)
        v2.extend_from_slice(&[0x00, 0x03, 0xFF]); // 0xFF x4

        let mut out_v3: Vec<u8> = Vec::new();
        process_cups_raster_to_spl(
            &no_args(),
            Box::new(Cursor::new(v3)),
            &mut out_v3,
            &current_service_date(),
        )
        .expect("v3 akışı işlenmeli");
        let mut out_v2: Vec<u8> = Vec::new();
        process_cups_raster_to_spl(
            &no_args(),
            Box::new(Cursor::new(v2)),
            &mut out_v2,
            &current_service_date(),
        )
        .expect("v2 akışı işlenmeli");

        assert!(!out_v3.is_empty());
        assert_eq!(out_v2, out_v3, "v2 ve v3 çıktıları ayrıştı");
    }

    /// `validate_page_header` sınırları, PPD'nin sunduğu HER seçeneği
    /// kapsamalı.
    ///
    /// Sabitler PPD'den elle türetilmişti ve aradaki bağ yalnızca bir
    /// yorumdu: PPD'ye daha büyük bir kağıt ya da daha yüksek bir çözünürlük
    /// eklenirse, sabitleri güncellemeyi unutmak meşru işlerin sessizce
    /// reddedilmesine yol açardı. Bu test o bağı zorunlu kılar.
    #[test]
    fn test_limits_cover_every_ppd_option() {
        for (x_dpi, y_dpi) in ppd_resolutions() {
            assert!(
                SplResolution::pair_is_supported(x_dpi, y_dpi),
                "PPD {}x{} DPI sunuyor ama filtre bu çifti desteklemiyor",
                x_dpi,
                y_dpi
            );
            for dpi in [x_dpi, y_dpi] {
                assert!(
                    dpi <= MAX_DPI,
                    "PPD {} DPI sunuyor ama MAX_DPI = {}; sabiti güncelleyin",
                    dpi,
                    MAX_DPI
                );
            }
        }

        for (name, w, h) in ppd_paper_dimensions() {
            for pt in [w, h] {
                assert!(
                    pt <= MAX_POINTS,
                    "PPD '{}' {} pt kağıt sunuyor ama MAX_POINTS = {}; sabiti güncelleyin",
                    name,
                    pt,
                    MAX_POINTS
                );
                // En büyük kağıt en yüksek çözünürlükte satır/sütun
                // sınırlarına da sığmalı.
                let pixels = (pt as u64 * MAX_DPI as u64).div_ceil(72);
                assert!(
                    pixels <= MAX_LINES as u64,
                    "{} pt @ {} DPI = {} satır, MAX_LINES = {}",
                    pt,
                    MAX_DPI,
                    pixels,
                    MAX_LINES
                );
                assert!(
                    pixels.div_ceil(8) <= MAX_BYTES_PER_LINE as u64,
                    "{} pt @ {} DPI = {} bayt/satır, MAX_BYTES_PER_LINE = {}",
                    pt,
                    MAX_DPI,
                    pixels.div_ceil(8),
                    MAX_BYTES_PER_LINE
                );
            }
        }
    }

    #[test]
    fn test_validate_page_header_rejects_line_wider_than_page() {
        let mut header = valid_header();
        header.page_size_points = [297, 420]; // desteklenen A6
        header.width = 4960;
        header.bytes_per_line = 620; // cupsWidth ile tutarlı, sayfayla değil
        let err = validate_page_header(&header).expect_err("dar sayfa reddedilmeliydi");
        assert!(
            err.to_string().contains("sayfa genişliğine sığmıyor"),
            "hata nedeni açıklanmalı: {}",
            err
        );
    }

    /// Gerçek bir tam genişlik A4 sayfası (595 pt @ 600 DPI = 620 B/satır)
    /// reddedilmemeli — aşırı düzeltme kontrolü.
    #[test]
    fn test_validate_page_header_accepts_full_width_a4() {
        let mut header = valid_header();
        header.width = 4960;
        header.bytes_per_line = 620;
        assert!(validate_page_header(&header).is_ok());
    }

    /// Yuvarlama payı: 1 baytlık aşım hoş görülür, 2 bayt reddedilir.
    #[test]
    fn test_validate_page_header_line_width_slack_is_one_byte() {
        // 595 pt @ 600 DPI => 4960 px => 620 bayt bant genişliği.
        let mut ok = valid_header();
        ok.width = 4968; // 621 bayt
        ok.bytes_per_line = 621;
        assert!(
            validate_page_header(&ok).is_ok(),
            "1 baytlık pay kabul edilmeli"
        );

        let mut too_wide = valid_header();
        too_wide.width = 4976; // 622 bayt
        too_wide.bytes_per_line = 622;
        assert!(
            validate_page_header(&too_wide).is_err(),
            "2 bayt aşım reddedilmeli"
        );
    }

    /// D-02: `cupsHeight` sayfanın fiziksel yüksekliğine sığmalı — D-01'in
    /// dikey karşılığı. Yamadan önce bu başlık kabul ediliyordu.
    #[test]
    fn test_validate_page_header_rejects_page_taller_than_paper() {
        let mut header = valid_header();
        header.page_size_points = [297, 420]; // desteklenen A6
        header.height = 4_000; // MAX_LINES içinde, ama A6'ya sığmıyor
        let err = validate_page_header(&header).expect_err("uzun sayfa reddedilmeliydi");
        assert!(
            err.to_string().contains("sayfa yüksekliğine sığmıyor"),
            "hata nedeni açıklanmalı: {}",
            err
        );

        // Normal boyutlu bir A4 sayfasında da aşım yakalanmalı: 842 pt @ 600
        // DPI = 7017 satır; raster bunun altında kalmalıdır.
        let mut a4 = valid_header();
        a4.height = 24_000;
        assert!(
            validate_page_header(&a4).is_err(),
            "A4'e sığmayan yükseklik reddedilmeli"
        );
    }

    /// Aşırı düzeltme kontrolü: gerçek `cupsfilter` çıktısının ürettiği
    /// yükseklikler reddedilmemeli. Değerler, ppd/samsung-ml2160.ppd ile
    /// `cupsfilter -m application/vnd.cups-raster` çalıştırılarak ölçüldü;
    /// hepsi fiziksel sınırın altında kalır çünkü `*ImageableArea` kenar
    /// boşlukları (12 pt üst + 12 pt alt) düşülür.
    #[test]
    fn test_validate_page_header_accepts_real_cupsfilter_heights() {
        // (sayfa_genişliği_pt, sayfa_yüksekliği_pt, y_dpi, ölçülen cupsHeight)
        let measured = [
            (595u32, 842u32, 300u32, 3408u32), // A4
            (595, 842, 600, 6817),
            (595, 842, 1200, 13633),
            (612, 792, 600, 6400),  // Letter
            (612, 1008, 600, 8200), // Legal
            (612, 1008, 1200, 16400),
            (297, 420, 600, 3300),   // A6
            (595, 935, 1200, 15183), // Folio
        ];
        for (width_pt, height_pt, ydpi, cups_height) in measured {
            let mut h = valid_header();
            h.page_size_points = [width_pt, height_pt];
            // Bu tablodaki ölçümler simetrik çözünürlüklerden alındı.
            h.hw_resolution = [ydpi, ydpi];
            h.height = cups_height;
            assert!(
                validate_page_header(&h).is_ok(),
                "gerçek cupsfilter çıktısı reddedildi: {}x{} pt @ {} DPI => {} satır",
                width_pt,
                height_pt,
                ydpi,
                cups_height
            );
        }
    }

    /// Yuvarlama payı: 8 satırlık aşım hoş görülür, fazlası reddedilir.
    #[test]
    fn test_validate_page_header_height_slack_is_eight_lines() {
        // 842 pt @ 600 DPI => ceil(842 * 600 / 72) = 7017 satır.
        let exact = compute_page_height_lines(842, 600);
        assert_eq!(exact, 7017);

        let mut ok = valid_header();
        ok.height = exact + 8;
        assert!(
            validate_page_header(&ok).is_ok(),
            "8 satırlık pay kabul edilmeli"
        );

        let mut too_tall = valid_header();
        too_tall.height = exact + 9;
        assert!(
            validate_page_header(&too_tall).is_err(),
            "9 satır aşım reddedilmeli"
        );
    }

    /// D-06: sert kenar boşluğu SpliX ile birebir aynı hesaplanmalı.
    ///
    /// SpliX compress.cpp: `((ceil(marginPt * dpi / 72) + 7) & ~7) / 8`.
    /// 8'e YUKARI hizalama önemli: 12 pt @600 DPI = 100 piksel, hizalanınca
    /// 104 piksel = 13 bayt olur; hizalamasız 12,5 bayt (kırpılınca 12) çıkar
    /// ve bant bir bayt kayar.
    #[test]
    fn test_hard_margin_matches_splix_alignment() {
        assert_eq!(hard_margin_bytes(media::HARD_MARGIN_PT, 300), 7);
        assert_eq!(hard_margin_bytes(media::HARD_MARGIN_PT, 600), 14);
        assert_eq!(hard_margin_bytes(media::HARD_MARGIN_PT, 1200), 27);

        // Bu projenin PPD'sindeki *ImageableArea sol kenar boşluğu: 12 pt.
        assert_eq!(hard_margin_bytes(12.0, 600), 13);
        assert_eq!(hard_margin_bytes(12.0, 300), 7);
        assert_eq!(hard_margin_bytes(12.0, 1200), 25);
        // Kenar boşluğu bildirilmemişse kaydırma da yok.
        assert_eq!(hard_margin_bytes(0.0, 600), 0);
    }

    /// D-06 regresyonu: yatay yerleşim ORTALAMA EKSİ SERT KENAR BOŞLUĞU
    /// olmalıdır, yalnızca ortalama değil.
    ///
    /// Eskiden yalnızca `(bandWidthInB - lineSize) / 2` uygulanıyordu; A4 @600
    /// DPI'da bu 12 baytlık (96 piksel ≈ 4 mm) bir sağa kayma demekti, çünkü
    /// SpliX bandı doldururken `hardMarginXInB` (13 bayt) kadar ATLAR
    /// (compress.cpp:227). Net ofset sıfırdır.
    #[test]
    fn test_band_placement_subtracts_hard_margin() {
        // A4 @600 DPI, bu projenin PPD'sindeki gerçek sayılar:
        // bant 620 B, CUPS satırı 595 B, sert kenar boşluğu 13 B.
        let a4 = band_placement(620, 595, hard_margin_bytes(12.0, 600)).unwrap();
        assert_eq!(
            a4,
            BandPlacement {
                dst_offset: 0,
                src_skip: 1
            },
            "ortalama (12 B) sert kenar boşluğunu (13 B) telafi etmeli"
        );

        // Regresyon çapası: sert kenar boşluğu düşülmezse eski, hatalı
        // 12 baytlık kayma geri gelir.
        assert_eq!(
            band_placement(620, 595, 0).unwrap(),
            BandPlacement {
                dst_offset: 12,
                src_skip: 0
            },
            "kenar boşluğu yoksa davranış saf ortalamadır"
        );

        // Ortalama sert kenar boşluğundan BÜYÜKSE fark hedefe kalır.
        assert_eq!(
            band_placement(620, 560, 13).unwrap(),
            BandPlacement {
                dst_offset: 17,
                src_skip: 0
            }
        );

        // Bant satırdan darsa ortalama yoktur; kenar boşluğu satırdan atılır.
        assert_eq!(
            band_placement(600, 620, 13).unwrap(),
            BandPlacement {
                dst_offset: 0,
                src_skip: 13
            }
        );

        // Satırın tamamı atlanacaksa geometri tutarsızdır: sessizce kırpmak
        // yerine reddedilir (eskiden `src_skip` 3'e kırpılıp tek baytlık,
        // yani fiilen boş bir sayfa üretiliyordu).
        let err = band_placement(4, 4, 999)
            .expect_err("satırdan geniş sert kenar boşluğu reddedilmeliydi");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("Sert kenar boşluğu"), "{}", err);

        // Sınır: tam olarak bir bayt kalması hâlâ kabul edilir.
        assert_eq!(
            band_placement(4, 4, 3).unwrap(),
            BandPlacement {
                dst_offset: 0,
                src_skip: 3
            }
        );
    }

    /// D-07: `Margins[0]` doğrulanmadığında `hard_margin_bytes` içindeki
    /// `px + 7` toplaması taşıyordu — `overflow-checks` açık yapılarda iş
    /// ortasında panik, sürüm yapılarında sessizce 0'a sarma. Sayfadan geniş
    /// bir sol kenar boşluğu artık başlık doğrulamasında reddediliyor.
    #[test]
    fn test_validate_page_header_rejects_out_of_page_left_margin() {
        for margin in [595, 600, 300_000_000] {
            let mut header = valid_header();
            header.margins[0] = margin;
            let err = validate_page_header(&header)
                .expect_err("sayfadan geniş sol kenar boşluğu reddedilmeliydi");
            assert!(
                err.to_string().contains("Geçersiz sol kenar boşluğu"),
                "{} pt için yanlış hata: {}",
                margin,
                err
            );
        }

        // PPD'nin gerçek değeri (12 pt) ve sınırın hemen altı kabul edilmeli.
        for margin in [0, 12, 594] {
            let mut header = valid_header();
            header.margins[0] = margin;
            assert!(
                validate_page_header(&header).is_ok(),
                "{} pt kabul edilmeliydi",
                margin
            );
        }
    }

    /// D-06 regresyonu (uçtan uca): raster içeriği, yazıcıya giden bant
    /// tamponunda sert kenar boşluğu düşülmüş sütunda durmalı.
    ///
    /// Gerçek A4 @600 DPI geometrisi kurulur (620 B bant, 595 B CUPS satırı,
    /// `Margins[0] = 12 pt`) ve satırın 3. baytına bir işaret konur.
    /// Beklenen sütun `3 - src_skip + dst_offset = 2`'dir; düzeltmeden önce
    /// aynı bayt 15. sütuna (12 baytlık kayma + 3) yazılıyordu.
    #[test]
    fn test_content_lands_at_hard_margin_corrected_column() {
        const MARKER_INDEX: usize = 3;
        let mut spec = RasterSpec::a4(600, 600, 8);
        spec.width_px = 4760; // 595 B/satır: gerçek cupsfilter çıktısına eşit
        spec.margin_left_pt = 12; // PPD *ImageableArea: "12 12 583 830"

        let mut pattern = vec![0u8; 595];
        pattern[MARKER_INDEX] = 0xFF;
        spec.line_pattern = Some(pattern);

        let out = run_filter(spec.build());
        let band = first_band_buffer(&out);
        assert_eq!(band.len(), 620 * QPDL_BAND_HEIGHT, "bant tamponu boyutu");

        let columns = nonzero_columns_in_first_line(&band, QPDL_BAND_HEIGHT);
        assert_eq!(
            columns,
            vec![1],
            "işaret baytı yanlış sütunda; 15 ise sert kenar boşluğu düşülmüyor"
        );
    }

    /// Missing integer header margins must not remove the 12.5 pt driver margin.
    #[test]
    fn test_missing_header_margin_still_uses_driver_constant() {
        const MARKER_INDEX: usize = 3;
        let mut spec = RasterSpec::a4(600, 600, 8);
        spec.width_px = 4760;
        spec.margin_left_pt = 0;

        let mut pattern = vec![0u8; 595];
        pattern[MARKER_INDEX] = 0xFF;
        spec.line_pattern = Some(pattern);

        let out = run_filter(spec.build());
        let band = first_band_buffer(&out);
        let columns = nonzero_columns_in_first_line(&band, QPDL_BAND_HEIGHT);
        assert_eq!(columns, vec![MARKER_INDEX - 2]);
    }

    /// Yatay ve dikey eksen AYNI origin'i kullanmalı.
    ///
    /// Hatanın özü buydu: dikeyde hiçbir kaydırma yokken yatayda 12 baytlık
    /// bir kaydırma vardı. SpliX'te iki eksenin de neti sıfırdır (ortalama
    /// eksi sert kenar boşluğu). Bu test ilk raster satırının bant tamponunun
    /// hem 0. satırında hem 0. sütununda başladığını sabitler.
    #[test]
    fn test_horizontal_and_vertical_origins_agree() {
        let mut spec = RasterSpec::a4(600, 600, 8);
        spec.width_px = 4760;
        spec.margin_left_pt = 12;
        // With 14 margin bytes and 12 centring bytes, source byte 2 lands at column 0.
        let mut pattern = vec![0u8; 595];
        pattern[2] = 0xFF;
        spec.line_pattern = Some(pattern);

        let out = run_filter(spec.build());
        let band = first_band_buffer(&out);

        // Sütun 0, satır 0: içerik bandın sol-üst köşesinden başlar.
        assert_eq!(band[0], 0xFF, "içerik bandın (0,0) köşesinde başlamalı");
        assert_eq!(
            nonzero_columns_in_first_line(&band, QPDL_BAND_HEIGHT),
            vec![0]
        );
    }

    /// Yükseklik sınırı dikey çözünürlüğe bağlı olmalı: `compute_page_height_lines`
    /// `hw_resolution[1]`'i alır, `[0]`'ı değil. 1200x600 desteklenen bir mod
    /// olduğundan iki eksenin birbirinden bağımsız kullanılması gerekir.
    #[test]
    fn test_page_height_lines_uses_vertical_resolution() {
        assert_eq!(compute_page_height_lines(842, 600), 7017);
        assert_eq!(compute_page_height_lines(842, 300), 3509);
        assert_eq!(compute_page_height_lines(842, 1200), 14034);

        // Sınır gerçekten çözünürlükle ölçekleniyor: 600 DPI'da geçerli olan
        // bir yükseklik, aynı kağıtta 300 DPI'da reddedilmeli.
        let mut h300 = valid_header();
        h300.hw_resolution = [300, 300];
        h300.width = 2480;
        h300.bytes_per_line = 310;
        h300.height = 6817; // 600 DPI'nın satır sayısı
        assert!(
            validate_page_header(&h300).is_err(),
            "300 DPI'da 600 DPI'nın satır sayısı kabul edilmemeli"
        );

        h300.height = 3408; // 300 DPI için gerçek cupsfilter değeri
        assert!(validate_page_header(&h300).is_ok());
    }

    /// Asimetrik çözünürlük (`1200x600dpi`) DESTEKLENEN gerçek bir QPDL
    /// modudur — SpliX de aynı seçeneği ml2010/ml2015/ml1640/ml2510/ml2525
    /// PPD'lerinde sunar — ve reddedilmemelidir.
    #[test]
    fn test_validate_page_header_accepts_asymmetric_resolution() {
        // A4 @ 1200x600: cupsfilter'ın gerçekte ürettiği değerler.
        let mut h = valid_header();
        h.hw_resolution = [1200, 600];
        h.width = 9517;
        h.bytes_per_line = 1190;
        h.height = 6817;
        assert!(
            validate_page_header(&h).is_ok(),
            "1200x600dpi gerçek bir QPDL modu, reddedilmemeli: {:?}",
            validate_page_header(&h).err()
        );
    }

    /// PPD'nin sunduğu her çözünürlük, filtre tarafından da kabul edilmeli:
    /// aksi hâlde kullanıcı sebebi belirsiz bir "filter failed" görür.
    #[test]
    fn test_filter_accepts_every_ppd_resolution() {
        for (x, y) in ppd_resolutions() {
            // O çözünürlükte A4 için tutarlı bir başlık kur.
            let mut h = valid_header();
            h.hw_resolution = [x, y];
            h.bytes_per_line = (595 * x).div_ceil(72).div_ceil(8);
            h.width = h.bytes_per_line * 8;
            h.height = (842 * y).div_ceil(72);
            assert!(
                validate_page_header(&h).is_ok(),
                "PPD {}x{} DPI sunuyor ama filtre reddediyor: {:?}",
                x,
                y,
                validate_page_header(&h).err()
            );
        }
    }

    /// D-04 regresyonu: `cupsColorOrder` artık denetleniyor.
    #[test]
    fn test_validate_page_header_checks_color_order() {
        // 1-bit tek kanallı veride üç dizilim de eşdeğerdir, üçü de kabul.
        for order in [
            CupsColorOrder::Chunked,
            CupsColorOrder::Banded,
            CupsColorOrder::Planar,
        ] {
            let mut header = valid_header();
            header.color_order = order;
            assert!(
                validate_page_header(&header).is_ok(),
                "{:?} kabul edilmeliydi",
                order
            );
        }

        let mut unknown = valid_header();
        unknown.color_order = CupsColorOrder::Unknown(99);
        assert!(
            validate_page_header(&unknown).is_err(),
            "tanınmayan dizilim reddedilmeli"
        );
    }

    /// Belirtilen sayıda küçük, geçerli sayfadan oluşan bir V3 akışı üretir.
    fn v3_multipage_stream(pages: u32) -> Vec<u8> {
        v3_multipage_stream_with_copies(pages, 0)
    }

    /// `v3_multipage_stream`'in, sayfa başlığındaki `cupsNumCopies` alanını da
    /// ayarlayan sürümü; yaprak (sayfa x kopya) bütçesini sınamak için.
    fn v3_multipage_stream_with_copies(pages: u32, copies: u32) -> Vec<u8> {
        let mut page = vec![0u8; 1796];
        {
            let mut put = |off: usize, val: u32| {
                page[off..off + 4].copy_from_slice(&val.to_be_bytes());
            };
            // Desteklenen en küçük kâğıt ve çözünürlük (A6 @ 300 DPI), fakat
            // yalnızca 8x1 piksellik geçerli bir raster bölgesi. Sayfa başına
            // bant tamponu yaklaşık 10 KiB'ta kalır; 5.000 sayfalık sınır
            // testi yine hızlıdır.
            put(276, 300); // hw_resolution
            put(280, 300);
            put(352, 297); // page_size_points: A6
            put(356, 420);
            put(372, 8); // width
            put(376, 1); // height
            put(384, 1); // bits_per_color
            put(388, 1); // bits_per_pixel
            put(392, 1); // bytes_per_line
            put(400, 3); // color_space = K
            put(340, copies); // num_copies
        }
        let mut stream = b"RaS3".to_vec();
        for _ in 0..pages {
            stream.extend_from_slice(&page);
            stream.push(0u8); // 1 satır x 1 bayt
        }
        stream
    }

    /// D-04 regresyonu: sayfa sayısının bir üst sınırı olmalı.
    #[test]
    fn test_page_count_is_capped() {
        let mut out: Vec<u8> = Vec::new();
        let err = process_cups_raster_to_spl(
            &no_args(),
            Box::new(Cursor::new(v3_multipage_stream(MAX_PAGES_PER_JOB + 1))),
            &mut out,
            &current_service_date(),
        )
        .expect_err("sayfa sınırı aşılınca hata beklenir");
        assert!(err.to_string().contains("sayfa sınırını aştı"), "{}", err);
        // Sınır aşılsa bile iş düzgün kapatılmalı (Y-03 garantisi).
        assert!(out.ends_with(spl::PJL_END));
    }

    /// Ham raster hacmi bütçesi uygulanmalı ve tam sınırda kabul etmeli.
    #[test]
    fn test_job_raster_byte_budget_is_enforced() {
        let mut budget = JobBudget::default();
        budget
            .account_page(MAX_JOB_RASTER_BYTES, 1)
            .expect("bütçenin tamamı kabul edilmeli");
        assert_eq!(budget.raster_bytes, MAX_JOB_RASTER_BYTES);

        let err = budget
            .account_page(1, 1)
            .expect_err("bütçeyi bir bayt aşmak hata vermeli");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("ham raster hacmi"), "{}", err);

        // Sarma yerine doyma: sınırlar ileride büyütülse bile taşma sessizce
        // bütçeyi sıfırlamamalı.
        let mut huge = JobBudget {
            pages: 0,
            raster_bytes: u64::MAX - 1,
            impressions: 0,
        };
        assert!(huge.account_page(u64::MAX, 1).is_err());
        assert_eq!(huge.raster_bytes, u64::MAX);
    }

    /// BULGU 2: sayfa sınırı ile kopya sınırının ÇARPIMI sınırsız olmamalı.
    ///
    /// Bu testin varlık nedeni, iki sınırın ayrı ayrı "makul" görünürken
    /// birlikte 4.995.000 yaprağa (eski değerlerle) izin vermesiydi. Sayaç
    /// yaprak cinsindendir; sayfa sayısı tek başına anlamlı bir tavan değil.
    #[test]
    fn test_page_and_copy_limits_cannot_multiply_without_bound() {
        let unbounded_product = MAX_PAGES_PER_JOB as u64 * MAX_REALISTIC_COPIES as u64;
        assert!(
            MAX_JOB_IMPRESSIONS < unbounded_product,
            "yaprak sınırı, sayfa x kopya çarpımından ({}) küçük olmalı; \
             aksi hâlde hiçbir şey sınırlamıyor demektir",
            unbounded_product
        );

        // Azami kopya sayısıyla, yaprak bütçesinin izin verdiği sayfa sayısı.
        let mut budget = JobBudget::default();
        let allowed = MAX_JOB_IMPRESSIONS / MAX_REALISTIC_COPIES as u64;
        for page in 1..=allowed {
            budget
                .account_page(1, MAX_REALISTIC_COPIES)
                .unwrap_or_else(|e| panic!("sayfa {} reddedilmemeliydi: {}", page, e));
        }
        let err = budget
            .account_page(1, MAX_REALISTIC_COPIES)
            .expect_err("yaprak sınırı aşılınca hata beklenir");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(err.to_string().contains("yaprak sınırını aştı"), "{}", err);
    }

    /// Yaprak bütçesi tam sınırda kabul etmeli, bir yaprak fazlasında reddetmeli.
    #[test]
    fn test_impression_budget_is_not_off_by_one() {
        let mut budget = JobBudget::default();
        budget
            .account_page(1, u16::try_from(MAX_JOB_IMPRESSIONS).unwrap())
            .expect("tam sınır kadar yaprak kabul edilmeli");
        assert_eq!(budget.impressions, MAX_JOB_IMPRESSIONS);
        assert!(budget.account_page(1, 1).is_err());
    }

    /// Bütçe, HAM `num_copies` ile değil `sanitize_copies`'ten geçmiş değerle
    /// saymalı: yazıcıya gitmeyen kopyalar bütçeyi tüketmemeli, ama 65536 gibi
    /// bir değer de "0 kopya" sayılıp bütçeden kaçmamalı.
    #[test]
    fn test_impression_budget_counts_sanitized_copies() {
        let mut budget = JobBudget::default();
        // Eski `as u16` davranışında bu 0 ediyordu; şimdi 999 sayılmalı.
        budget.account_page(1, sanitize_copies(65_536)).unwrap();
        assert_eq!(budget.impressions, MAX_REALISTIC_COPIES as u64);

        let mut zero = JobBudget::default();
        zero.account_page(1, sanitize_copies(0)).unwrap();
        assert_eq!(zero.impressions, 1, "0 kopya en az 1 yaprak sayılmalı");
    }

    /// Yaprak sınırı uçtan uca da uygulanmalı ve iş yine düzgün kapanmalı.
    #[test]
    fn test_impression_limit_is_enforced_end_to_end() {
        // Her sayfa azami kopyayla: sınır sayfa sayısından çok önce dolar.
        let pages = (MAX_JOB_IMPRESSIONS / MAX_REALISTIC_COPIES as u64) as u32 + 1;
        assert!(
            pages < MAX_PAGES_PER_JOB,
            "sayfa sınırı önce devreye girmemeli"
        );

        let mut out: Vec<u8> = Vec::new();
        let err = process_cups_raster_to_spl(
            &no_args(),
            Box::new(Cursor::new(v3_multipage_stream_with_copies(
                pages,
                MAX_REALISTIC_COPIES as u32,
            ))),
            &mut out,
            &current_service_date(),
        )
        .expect_err("yaprak sınırı aşılınca hata beklenir");
        assert!(err.to_string().contains("yaprak sınırını aştı"), "{}", err);
        // Sınır aşılsa bile iş düzgün kapatılmalı (Y-03 garantisi).
        assert!(out.ends_with(spl::PJL_END));
    }

    /// Bütçe, normal çözünürlüklü işlerin davranışını DEĞİŞTİRMEMELİ:
    /// `MAX_PAGES_PER_JOB` kadar A4 @600 DPI sayfa bütçenin altında kalmalı.
    #[test]
    fn test_job_budget_does_not_bind_for_600dpi_pages_within_page_limit() {
        // Görüntülenebilir alanı değil daha büyük olan fiziksel A4 geometrisini
        // kullan; böylece sınır farklı cups-filters yuvarlamalarına da dayanır.
        let a4_600 = compute_page_width_pixels(595, 600).div_ceil(8) as u64
            * compute_page_height_lines(842, 600) as u64;
        let worst = a4_600 * MAX_PAGES_PER_JOB as u64;
        assert!(
            worst < MAX_JOB_RASTER_BYTES,
            "{} A4@600DPI sayfa ({} bayt) bütçeyi ({}) aşmamalı",
            MAX_PAGES_PER_JOB,
            worst,
            MAX_JOB_RASTER_BYTES
        );
    }

    /// Doğrulayıcının kabul ettiği en büyük ham raster geometrisi:
    /// Legal @1200 DPI ile D-01/D-02 yuvarlama payları. Bilinmeyen ölçüler
    /// artık kabul edilmediğinden global `MAX_POINTS` değeri erişilebilir bir
    /// sayfa geometrisi değildir.
    fn largest_accepted_page_geometry() -> (u64, u64) {
        let bytes_per_line = compute_page_width_pixels(612, 1200).div_ceil(8) as u64
            + LINE_OVERSHOOT_SLACK_BYTES as u64;
        let lines =
            compute_page_height_lines(1008, 1200) as u64 + HEIGHT_OVERSHOOT_SLACK_LINES as u64;
        (bytes_per_line * lines, lines)
    }

    /// ...ama en büyük kabul edilebilir sayfada GERÇEKTEN devreye girmeli,
    /// yoksa sayfa sınırının çözünürlük körlüğü kapanmamış olur.
    #[test]
    fn test_job_budget_binds_before_page_limit_at_max_page_size() {
        let (max_page, _) = largest_accepted_page_geometry();
        let pages_allowed = MAX_JOB_RASTER_BYTES / max_page;
        assert!(
            pages_allowed > 0,
            "bütçe tek bir azami sayfayı bile reddediyor"
        );
        assert!(
            pages_allowed < MAX_PAGES_PER_JOB as u64,
            "azami boyutlu sayfalarda bütçe sayfa sınırından önce devreye girmeli \
             (izin verilen: {}, sayfa sınırı: {})",
            pages_allowed,
            MAX_PAGES_PER_JOB
        );
        assert_eq!(
            pages_allowed + 1,
            401,
            "kaynak bütçesi açıklamasındaki ilk reddedilen sayfa değişti"
        );
    }

    /// BULGU 1: bütçe, en kötü durumdaki CPU süresini de sınırlamalı.
    ///
    /// Bu sınır bayt cinsinden olduğu için CPU süresini ancak sıkıştırıcının
    /// ölçülen hızı üzerinden dolaylı olarak bağlar. Aşağıdaki değer bu
    /// makinede, release derlemesinde, gerçek bant boyutunda (346.752 bayt)
    /// ölçüldü: sıkıştırılamaz gürültü ~6,65 MB/s (sıfır dolu bantta ~166 MB/s,
    /// yani en iyi ile en kötü arasında ~25 kat var).
    ///
    /// Testin amacı bir hız ölçmek değil — makineden makineye değişir — bütçe
    /// büyütüldüğünde bunun CPU tarafındaki bedelini görünür kılmak: filtre
    /// CUPS kuyruğunu tek iş parçacığıyla işlediği için bu süre boyunca
    /// sıradaki tüm işler bekler.
    const MEASURED_WORST_CASE_COMPRESS_BPS: u64 = 6_650_000;

    #[test]
    fn test_raster_budget_bounds_worst_case_cpu_time() {
        let worst_case_seconds = MAX_JOB_RASTER_BYTES / MEASURED_WORST_CASE_COMPRESS_BPS;
        assert!(
            worst_case_seconds <= 30 * 60,
            "en kötü durumda tek bir iş kuyruğu {} saniye bloke ediyor; \
             MAX_JOB_RASTER_BYTES ({}) düşürülmeli",
            worst_case_seconds,
            MAX_JOB_RASTER_BYTES
        );
    }

    /// BULGU 1'in asıl çekirdeği: bütçe ÇÖZÜLMÜŞ raster hacmini sayar, girdi
    /// hacmini değil — ve CUPS Raster v2'nin satır-RLE'si arada çok büyük bir
    /// genişleme sağlar. Desteklenen en büyük geometride yaklaşık 0,74 MiB'lik
    /// tamamen beyaz bir akış 8 GiB'tan fazla raster işi doğurabilir.
    ///
    /// Bu yüzden "girdi küçükse iş de küçüktür" varsayımı yapılamaz; tek gerçek
    /// savunma tavanın kendisidir. Test, o tavanın saldırganın gönderebileceği
    /// bayt sayısıyla DEĞİL, yalnızca sabitle sınırlı kaldığını pinliyor.
    #[test]
    fn test_compressed_input_cannot_amplify_past_the_raster_budget() {
        // Tek bir v2 satır kaydı 2 bayttır ([tekrar][0x80 = satır sonuna kadar
        // boşalt]) ve 256 satıra kadar üretir; sayfa başlığı 1796 bayt.
        let (page, lines) = largest_accepted_page_geometry();
        let input_per_page = 1796 + 2 * lines.div_ceil(256);
        let pages = MAX_JOB_RASTER_BYTES / page + 1;
        let attacker_bytes = pages * input_per_page;

        assert!(
            attacker_bytes < MAX_JOB_RASTER_BYTES / 1000,
            "genişleme oranı bu testin varsayımından düşük; ölçümü gözden geçirin"
        );

        // Asıl garanti: girdi ne kadar küçük olursa olsun, işlenen hacim
        // bütçeyi aşamaz.
        let mut budget = JobBudget::default();
        let mut processed = 0u64;
        for _ in 0..pages {
            match budget.account_page(page, 1) {
                Ok(_) => processed += page,
                Err(_) => break,
            }
        }
        assert!(
            processed <= MAX_JOB_RASTER_BYTES,
            "işlenen hacim ({}) bütçeyi ({}) aştı",
            processed,
            MAX_JOB_RASTER_BYTES
        );
    }

    /// Sınırın tam üstündeki bir iş sorunsuz işlenmeli.
    #[test]
    fn test_page_count_limit_is_not_off_by_one() {
        let mut out: Vec<u8> = Vec::new();
        process_cups_raster_to_spl(
            &no_args(),
            Box::new(Cursor::new(v3_multipage_stream(MAX_PAGES_PER_JOB))),
            &mut out,
            &current_service_date(),
        )
        .expect("tam sınır kadar sayfa kabul edilmeli");
        assert!(out.ends_with(spl::PJL_END));
    }

    #[test]
    fn test_validate_page_header_rejects_oversized_fields() {
        let mut over_dpi = valid_header();
        over_dpi.hw_resolution = [9999, 9999];
        assert!(validate_page_header(&over_dpi).is_err());

        let mut over_points = valid_header();
        over_points.page_size_points = [999_999, 999_999];
        assert!(validate_page_header(&over_points).is_err());

        let mut over_lines = valid_header();
        over_lines.height = 9_999_999;
        assert!(validate_page_header(&over_lines).is_err());

        let mut over_bpl = valid_header();
        over_bpl.width = 9_999_999;
        over_bpl.bytes_per_line = 9_999_999;
        assert!(validate_page_header(&over_bpl).is_err());
    }

    // ======================================================================
    // Şerit yüksekliği: SpliX compress.cpp `_compressBandedPage`
    //   if (xResolution == 300 && yResolution == 300) bandHeight /= 2;
    // ======================================================================

    #[test]
    fn test_band_height_is_halved_only_at_300x300() {
        let with = |x, y| {
            let mut h = valid_header();
            h.hw_resolution = [x, y];
            band_height_for(&h)
        };
        assert_eq!(
            with(300, 300),
            64,
            "300x300 DPI'da şerit yüksekliği 64 olmalı"
        );
        assert_eq!(with(600, 600), QPDL_BAND_HEIGHT);
        assert_eq!(with(1200, 1200), QPDL_BAND_HEIGHT);
        // Kural İKİ eksenin de 300 olmasını istiyor; asimetrik modlar 128'de kalır.
        assert_eq!(with(1200, 600), QPDL_BAND_HEIGHT);
        assert_eq!(with(300, 600), QPDL_BAND_HEIGHT);
        assert_eq!(with(600, 300), QPDL_BAND_HEIGHT);
    }

    /// Uçtan uca: 300 DPI bir iş, tele GERÇEKTEN 64 satırlık şerit kayıtları
    /// yazmalı. Regresyon değeri buradadır — `band_height_for` doğru olsa bile
    /// çağrı yerinde kullanılmazsa bu test kırılır.
    #[test]
    fn test_300dpi_job_writes_64_line_band_records() {
        let spec = RasterSpec::a4(300, 300, 200);
        let pages = parse_spl(&run_filter(spec.build()));
        assert_eq!(pages.len(), 1);
        let bands = &pages[0].bands;
        assert_eq!(
            bands.len(),
            200_usize.div_ceil(64),
            "200 satır / 64 = 4 şerit"
        );
        for (i, b) in bands.iter().enumerate() {
            assert_eq!(b.height_lines, 64, "şerit {} yüksekliği 64 olmalı", i);
            assert_eq!(b.index, i as u8);
        }
    }

    #[test]
    fn test_600dpi_job_still_writes_128_line_band_records() {
        let spec = RasterSpec::a4(600, 600, 200);
        let pages = parse_spl(&run_filter(spec.build()));
        let bands = &pages[0].bands;
        assert_eq!(
            bands.len(),
            200_usize.div_ceil(128),
            "200 satır / 128 = 2 şerit"
        );
        assert!(bands.iter().all(|b| b.height_lines == 128));
    }

    /// Asimetrik `1200x600dpi` modu 300 DPI kuralına takılmamalı.
    #[test]
    fn test_asymmetric_1200x600_keeps_128_line_bands() {
        let spec = RasterSpec::a4(1200, 600, 200);
        let pages = parse_spl(&run_filter(spec.build()));
        assert!(pages[0].bands.iter().all(|b| b.height_lines == 128));
    }

    /// PPD'nin sunduğu HER çözünürlük için şerit yüksekliği SpliX kuralıyla
    /// aynı olmalı. PPD'ye yeni bir çözünürlük eklenirse bu test onu kapsar.
    #[test]
    fn test_band_height_matches_splix_rule_for_every_ppd_resolution() {
        for (x, y) in ppd_resolutions() {
            let expected = if x == 300 && y == 300 { 64 } else { 128 };
            let mut h = valid_header();
            h.hw_resolution = [x, y];
            assert_eq!(
                band_height_for(&h),
                expected,
                "PPD {}x{} DPI sunuyor; SpliX kuralına göre şerit yüksekliği {} olmalı",
                x,
                y,
                expected
            );
        }
    }

    // ======================================================================
    // Duplex / tumble: SpliX request.cpp (mod seçimi) + qpdl.cpp (baytlar)
    // ======================================================================

    #[test]
    fn test_duplex_mode_maps_cups_duplex_and_tumble() {
        let mode = |duplex, tumble| {
            let mut h = valid_header();
            h.duplex = duplex;
            h.tumble = tumble;
            duplex_mode(&h)
        };
        assert_eq!(mode(false, false), SplDuplex::Simplex);
        assert_eq!(
            mode(false, true),
            SplDuplex::Simplex,
            "Duplex kapalıyken Tumble yok sayılır"
        );
        // ML-2160 ailesi `*QPDL ManualDuplex: "On"` bildirdiği için sonuç
        // daima Manual* olmalı; otomatik LongEdge/ShortEdge bu ailede yanlış.
        assert_eq!(mode(true, false), SplDuplex::ManualLongEdge);
        assert_eq!(mode(true, true), SplDuplex::ManualShortEdge);
    }

    /// Tek taraflı işlerde tumble baytı her sayfada 0, duplex baytı 1 olmalı.
    /// (SpliX: Simplex -> duplex = 1, tumble = 0.)
    #[test]
    fn test_simplex_pages_have_duplex_byte_one_and_no_tumble() {
        let mut spec = RasterSpec::a4(600, 600, 8);
        spec.pages = 3;
        let pages = parse_spl(&run_filter(spec.build()));
        assert_eq!(pages.len(), 3);
        for (i, p) in pages.iter().enumerate() {
            assert_eq!(
                p.header[0xB],
                1,
                "sayfa {}: Simplex duplex baytı 1 olmalı",
                i + 1
            );
            assert_eq!(
                p.header[0xC],
                0,
                "sayfa {}: Simplex tumble baytı 0 olmalı",
                i + 1
            );
        }
    }

    /// Elle duplex'te tumble, SAYFA NUMARASININ paritesidir ve sayaç 1'den
    /// başlar: tek numaralı sayfalarda 1, çift numaralılarda 0.
    /// Eski kod burada koşulsuz 0 yazıyordu.
    #[test]
    fn test_manual_duplex_tumble_alternates_from_page_one() {
        let mut spec = RasterSpec::a4(600, 600, 8);
        spec.duplex = true;
        spec.pages = 4;
        let pages = parse_spl(&run_filter(spec.build()));
        assert_eq!(pages.len(), 4);
        let tumbles: Vec<u8> = pages.iter().map(|p| p.header[0xC]).collect();
        assert_eq!(
            tumbles,
            vec![1, 0, 1, 0],
            "tumble = pageNr % 2 (pageNr 1 tabanlı)"
        );
        for p in &pages {
            assert_eq!(p.header[0xB], 0, "elle duplex'te duplex baytı 0 olmalı");
        }
    }

    /// Elle duplex PJL'de `DUPLEX=ON` değil `DUPLEX=MANUAL` demeli
    /// (SpliX printer.cpp sendPJLHeader).
    #[test]
    fn test_manual_duplex_job_sends_pjl_duplex_manual() {
        let mut spec = RasterSpec::a4(600, 600, 8);
        spec.duplex = true;
        let out = run_filter(spec.build());
        let pjl = String::from_utf8_lossy(&out[..out.len().min(512)]).into_owned();
        assert!(pjl.contains("@PJL SET DUPLEX=MANUAL\n"), "PJL: {}", pjl);
        assert!(pjl.contains("@PJL SET BINDING=LONGEDGE\n"), "PJL: {}", pjl);
        assert!(
            !pjl.contains("@PJL SET DUPLEX=ON"),
            "elle duplex ON bildirmemeli: {}",
            pjl
        );
    }

    #[test]
    fn test_short_edge_manual_duplex_sends_shortedge_binding() {
        let mut spec = RasterSpec::a4(600, 600, 8);
        spec.duplex = true;
        spec.tumble = true;
        let out = run_filter(spec.build());
        let pjl = String::from_utf8_lossy(&out[..out.len().min(512)]).into_owned();
        assert!(pjl.contains("@PJL SET DUPLEX=MANUAL\n"), "PJL: {}", pjl);
        assert!(pjl.contains("@PJL SET BINDING=SHORTEDGE\n"), "PJL: {}", pjl);
    }

    /// CUPS raster başlığındaki `Tumble` alanı 368. baytta (cupsWidth'ten
    /// hemen önce) okunmalı. Alan daha önce `turn_off` adıyla duruyordu ve
    /// hiç kullanılmadığı için yanlış adlandırma fark edilmiyordu.
    #[test]
    fn test_tumble_is_parsed_from_offset_368() {
        let mut spec = RasterSpec::a4(600, 600, 8);
        spec.duplex = true;
        spec.tumble = true;
        let stream = spec.build();
        let header = PageHeader::parse(&stream[4..4 + 1796], CupsRasterVersion::V3Be).unwrap();
        assert!(header.tumble, "368. bayttaki Tumble alanı okunmadı");
        assert!(header.duplex);
    }

    // ======================================================================
    // Kağıt boyutu eşlemesi
    // ======================================================================

    /// PPD'nin sunduğu her kağıt boyutu, QPDL'nin doğru kağıt koduna
    /// eşlenmeli. PPD ile tablo arasındaki her sapma, meşru bir işi reddeder;
    /// bu test iki listeyi birlikte güncel tutar.
    #[test]
    fn test_every_ppd_paper_size_maps_to_its_qpdl_code() {
        use spl::SplPaperSize;

        let expected = |name: &str| -> SplPaperSize {
            match name {
                "A4" => SplPaperSize::A4,
                "Letter" => SplPaperSize::Letter,
                "Legal" => SplPaperSize::Legal,
                "Executive" => SplPaperSize::Executive,
                "A5" => SplPaperSize::A5,
                "A6" => SplPaperSize::A6,
                "B5" => SplPaperSize::B5,
                "Env10" => SplPaperSize::Env10,
                "EnvDL" => SplPaperSize::Dl,
                "EnvC5" => SplPaperSize::C5,
                "Folio" => SplPaperSize::Folio,
                other => panic!("PPD'de tabloya eklenmemiş kağıt boyutu: {}", other),
            }
        };

        let papers = ppd_paper_dimensions();
        assert_eq!(
            papers.len(),
            11,
            "PPD'den beklenen sayıda kağıt boyutu okunamadı"
        );
        for (name, w, h) in papers {
            assert_eq!(
                SplPaperSize::from_dimensions_pt_exact(w, h),
                Some(expected(&name)),
                "PPD '{}' = {}x{} pt, ama kesin eşleme başka bir kod veriyor",
                name,
                w,
                h
            );
        }
    }

    /// Folio 210x330 mm'dir (595x935 pt). 612x936 pt olan 8.5x13 inç ölçüsü
    /// Adobe adlandırmasında FanFoldGermanLegal'dir ve Folio değildir;
    /// `cupstestppd` de PPD'yi tam bu gerekçeyle uyarıyordu.
    #[test]
    fn test_folio_is_f4_not_fanfold_german_legal() {
        use spl::SplPaperSize;
        assert_eq!(
            SplPaperSize::from_dimensions_pt_exact(595, 935),
            Some(SplPaperSize::Folio)
        );
        assert_eq!(
            SplPaperSize::from_dimensions_pt_exact(935, 595),
            Some(SplPaperSize::Folio)
        );
        // 8.5x13 inç Folio'ya ya da sessizce A4'e eşlenmemeli.
        assert_eq!(SplPaperSize::from_dimensions_pt_exact(612, 936), None);
    }

    // ======================================================================
    // Kağıt kaynağı (PPD *InputSlot -> CUPS MediaPosition -> QPDL 0x9 baytı)
    // ======================================================================

    /// PPD'nin sunduğu her kağıt kaynağı, QPDL'nin doğru kaynak koduna
    /// eşlenmeli.
    ///
    /// PPD `*InputSlot` seçeneklerini doğrudan QPDL kodlarıyla
    /// numaralandırıyor (`<</MediaPosition 1>>` = Auto). Bağ bir yorum
    /// olarak kalırsa, PPD'ye QPDL kodu olmayan bir değer eklendiğinde
    /// (eskiden Auto = 0 idi) seçenek sessizce Auto'ya düşer.
    #[test]
    fn test_every_ppd_input_slot_maps_to_its_qpdl_code() {
        use spl::SplPaperSource;

        let ppd = ppd_text();

        let expected = |name: &str| -> SplPaperSource {
            match name {
                "Auto" => SplPaperSource::Auto,
                "Manual" => SplPaperSource::Manual,
                "Multi" => SplPaperSource::Multi,
                "Upper" => SplPaperSource::Upper,
                "Lower" => SplPaperSource::Lower,
                other => panic!("PPD'de tabloya eklenmemiş kağıt kaynağı: {}", other),
            }
        };

        let mut checked = 0;
        for line in ppd.lines() {
            let Some(rest) = line.strip_prefix("*InputSlot ") else {
                continue;
            };
            let (name, code) = rest.split_once(':').expect("bozuk *InputSlot satırı");
            let name = name.split('/').next().unwrap().trim();
            // "<</MediaPosition 2>>setpagedevice" -> 2
            let pos: u32 = code
                .split("MediaPosition")
                .nth(1)
                .expect("MediaPosition yok")
                .trim_start()
                .split(|c: char| !c.is_ascii_digit())
                .next()
                .unwrap()
                .parse()
                .expect("MediaPosition sayı değil");

            assert_eq!(
                SplPaperSource::from_media_position(pos),
                Some(expected(name)),
                "PPD '{}' = MediaPosition {}, ama filtre başka bir koda eşliyor",
                name,
                pos
            );
            checked += 1;
        }
        assert_eq!(
            checked, 2,
            "PPD'den beklenen sayıda kağıt kaynağı okunamadı"
        );
    }

    /// Uçtan uca: "Manual Feeder" seçimi QPDL sayfa başlığının 0x9 baytına
    /// ulaşmalı. Yamadan önce bu bayt koşulsuz 1 (Auto) idi.
    #[test]
    fn test_input_slot_reaches_qpdl_page_header() {
        for (media_position, expected) in [(1u32, 1u8), (2, 2), (0, 1)] {
            let mut spec = RasterSpec::a4(600, 600, 8);
            spec.media_position = media_position;
            let pages = parse_spl(&run_filter(spec.build()));
            assert_eq!(
                pages[0].header[0x9], expected,
                "MediaPosition {} -> QPDL kaynak kodu {} olmalı",
                media_position, expected
            );
        }
    }

    /// Tanınmayan bir `MediaPosition` sessizce yanlış bir koda dönüşmemeli;
    /// Auto'ya düşmeli.
    #[test]
    fn test_unknown_media_position_falls_back_to_auto() {
        use spl::SplPaperSource;
        assert_eq!(SplPaperSource::from_media_position(6), None);
        assert_eq!(SplPaperSource::from_media_position(u32::MAX), None);

        let mut spec = RasterSpec::a4(600, 600, 8);
        spec.media_position = 6;
        let pages = parse_spl(&run_filter(spec.build()));
        assert_eq!(pages[0].header[0x9], 1, "tanınmayan kaynak Auto'ya düşmeli");
    }

    // ======================================================================
    // Kağıt türü (PPD *MediaType -> CUPS MediaType -> @PJL SET PAPERTYPE)
    // ======================================================================

    /// PPD'nin sunduğu HER kağıt türü anahtarı, filtre tarafından da
    /// tanınmalı. Aksi hâlde kullanıcının seçimi sessizce `OFF`'a düşer ve
    /// zarf/etiket düz kağıt füzer ayarlarıyla basılır.
    #[test]
    fn test_every_ppd_media_type_is_accepted_by_the_filter() {
        let ppd = ppd_text();

        let mut checked = 0;
        for line in ppd.lines() {
            let Some(rest) = line.strip_prefix("*MediaType ") else {
                continue;
            };
            let (name, code) = rest.split_once(':').expect("bozuk *MediaType satırı");
            let name = name.split('/').next().unwrap().trim();

            assert_eq!(
                spl::pjl_paper_type(name),
                Some(name),
                "PPD '{}' sunuyor ama filtrenin PJL sözlüğünde yok",
                name
            );
            // Seçim raster başlığına ulaşmalı: PostScript kodu MediaType
            // dizesini anahtarın KENDİSİYLE ayarlamalı, aksi hâlde filtre
            // farklı bir değer görür.
            assert!(
                code.contains(&format!("MediaType({})", name)),
                "PPD '{}' seçimi raster başlığına aynı anahtarla ulaşmıyor: {}",
                name,
                line
            );
            checked += 1;
        }
        assert_eq!(
            checked,
            spl::PJL_PAPER_TYPES.len(),
            "PPD, yazıcının PJL sözlüğündeki türlerin tamamını sunmuyor"
        );
    }

    /// Uçtan uca: seçilen kağıt türü PJL başlığına ulaşmalı. Yamadan önce
    /// burada her zaman `PAPERTYPE=OFF` yazıyordu.
    #[test]
    fn test_media_type_reaches_pjl_papertype() {
        for media_type in ["ENV", "LABEL", "THICK", "OFF"] {
            let mut spec = RasterSpec::a4(600, 600, 8);
            spec.media_type = media_type;
            let out = run_filter(spec.build());
            let pjl = String::from_utf8_lossy(&out[..out.len().min(512)]).into_owned();
            assert!(
                pjl.contains(&format!("@PJL SET PAPERTYPE={}\n", media_type)),
                "{} PJL'e ulaşmadı: {}",
                media_type,
                pjl
            );
        }
    }

    /// Tanınmayan ya da boş bir `MediaType` güvenli varsayılana düşmeli ve
    /// asla ham olarak PJL satırına yazılmamalı (satır tırnaksızdır: bir
    /// boşluk ya da CR/LF komutu bozardı).
    #[test]
    fn test_unknown_media_type_falls_back_to_papertype_off() {
        for media_type in ["", "Envelope", "Plain", "EVIL VALUE"] {
            let mut spec = RasterSpec::a4(600, 600, 8);
            spec.media_type = media_type;
            let out = run_filter(spec.build());
            let pjl = String::from_utf8_lossy(&out[..out.len().min(512)]).into_owned();
            assert!(
                pjl.contains("@PJL SET PAPERTYPE=OFF\n"),
                "{:?} için OFF'a düşülmedi: {}",
                media_type,
                pjl
            );
            assert!(
                !pjl.contains("PAPERTYPE=EVIL"),
                "güvenilmez değer PJL satırına sızdı: {}",
                pjl
            );
        }
    }

    /// PPD varsayılanları filtrenin varsayılanlarıyla uyuşmalı: PPD
    /// `*DefaultMediaType: OFF` / `*DefaultInputSlot: Auto` diyorsa, hiçbir
    /// seçim yapılmamış bir iş de aynı sonucu üretmeli.
    #[test]
    fn test_ppd_defaults_match_filter_defaults() {
        let ppd = ppd_text();

        let default_of = |key: &str| -> String {
            ppd.lines()
                .find_map(|l| l.strip_prefix(key))
                .unwrap_or_else(|| panic!("{} yok", key))
                .trim()
                .to_string()
        };
        assert_eq!(default_of("*DefaultMediaType:"), spl::PJL_PAPERTYPE_DEFAULT);
        assert_eq!(default_of("*DefaultInputSlot:"), "Auto");
    }
}
