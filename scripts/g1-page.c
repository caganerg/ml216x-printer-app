/* SPDX-License-Identifier: GPL-2.0-only */
/* Generate the G-1 measurement page: full-media PWG Raster whose printed
 * result can be measured with a millimetre ruler.
 *
 * Build: cc -Wall -Wextra -Werror scripts/g1-page.c -lcups -lm -o g1-page
 * Usage: g1-page out.pwg pwg-media xdpi ydpi
 *
 * Why this page and not `scripts/pwg-probe-input.c`: that generator draws
 * single-pixel marks, which at 600 dpi are 42 um across. They prove the
 * callbacks carry full media, which is what P5 needed, but no ruler resolves
 * them and no toner reliably renders them. Gate G-1 is a physical measurement
 * (docs/GOLDEN-VALIDATION.md), so this page draws rulers instead.
 *
 * The geometry the driver applies, and which every position here is derived
 * from rather than assumed (crates/ml216x-printer-app/src/driver.rs
 * `page_geometry`, crates/spl2-core/src/geometry.rs):
 *
 *   horizontal  the first `hard_margin_bytes(12.5 pt, xdpi) * 8` pixel columns
 *               of the incoming line are dropped by `band_placement`, and the
 *               next column becomes band column 0 -- the engine's first
 *               printable column.
 *   vertical    the first `hard_margin_lines(12.5 pt, ydpi)` scanlines are
 *               dropped and the page is cut to `height - 2 * skip`.
 *
 * So raster pixel (drop_x, skip_y) is the first pixel that reaches paper, and
 * the whole measurement is: where on the sheet does that pixel actually land?
 * The PPD says 12.5 pt = 4.41 mm from each edge. Note that the horizontal drop
 * is rounded up to a byte column, so it is 4.74 mm of raster at 600 dpi rather
 * than 4.41 -- the sheet content is therefore predicted to sit 0.33 mm further
 * left than its nominal sheet position, and this program reports that shift in
 * its JSON rather than hiding it in the drawing.
 *
 * Every tick is placed at a whole millimetre of PREDICTED PHYSICAL distance
 * from a paper edge, so the maintainer lays a ruler with 0 on the paper edge
 * and reads the error off directly. Ticks that fall inside the margin are
 * emitted too: they are predicted to be clipped, and if one shows up on paper
 * the crop is smaller than modelled.
 */
#include <cups/raster.h>
#include <cups/pwg.h>
#include <fcntl.h>
#include <math.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

/* 12.5 pt in hundredths of a millimetre, the same 441 the P5 generator and
 * `media-col` use. crates/spl2-core/src/media.rs holds the pt value. */
#define MARGIN_HMM 441u
#define MARGIN_PT 12.5

/* Ruler ticks, in hundredths of a millimetre. */
#define TICK_STEP_HMM 100u   /* one tick per millimetre */
#define TICK_MAX_HMM 2500u   /* ... out to 25 mm, subject to the medium */
#define TICK_MINOR_HMM 400u  /* tick length: plain */
#define TICK_MEDIUM_HMM 800u /* ... every 5 mm */
#define TICK_MAJOR_HMM 1200u /* ... every 10 mm */
#define TICK_THICK_HMM 12u   /* 0.12 mm, at least 2 px */

#define BRACKET_ARM_HMM 1500u
#define BRACKET_THICK_HMM 100u

#define GLYPH_W 5
#define GLYPH_H 7

/* A 5x7 bitmap font, one entry per row, the low 5 bits used, MSB leftmost.
 * Only the characters the caption needs; anything else prints as a space. */
static const unsigned char FONT[][GLYPH_H] = {
    {0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00}, /* space */
    {0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E}, /* 0 */
    {0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E}, /* 1 */
    {0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F}, /* 2 */
    {0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E}, /* 3 */
    {0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02}, /* 4 */
    {0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E}, /* 5 */
    {0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E}, /* 6 */
    {0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08}, /* 7 */
    {0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E}, /* 8 */
    {0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C}, /* 9 */
    {0x0E, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11}, /* A */
    {0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E}, /* B */
    {0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E}, /* C */
    {0x1E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x1E}, /* D */
    {0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F}, /* E */
    {0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10}, /* F */
    {0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F}, /* G */
    {0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11}, /* H */
    {0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E}, /* I */
    {0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C}, /* J */
    {0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11}, /* K */
    {0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F}, /* L */
    {0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11}, /* M */
    {0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11}, /* N */
    {0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E}, /* O */
    {0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10}, /* P */
    {0x0E, 0x11, 0x11, 0x11, 0x15, 0x13, 0x0F}, /* Q */
    {0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11}, /* R */
    {0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E}, /* S */
    {0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04}, /* T */
    {0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E}, /* U */
    {0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04}, /* V */
    {0x11, 0x11, 0x11, 0x15, 0x15, 0x1B, 0x11}, /* W */
    {0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11}, /* X */
    {0x11, 0x11, 0x0A, 0x04, 0x04, 0x04, 0x04}, /* Y */
    {0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F}, /* Z */
    {0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C}, /* . */
    {0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00}, /* - */
    {0x00, 0x0C, 0x0C, 0x00, 0x0C, 0x0C, 0x00}, /* : */
    {0x01, 0x02, 0x02, 0x04, 0x08, 0x08, 0x10}, /* / */
    {0x00, 0x04, 0x04, 0x1F, 0x04, 0x04, 0x00}, /* + */
    {0x00, 0x00, 0x1F, 0x00, 0x1F, 0x00, 0x00}, /* = */
};
static const char FONT_CHARS[] = " 0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZ.-:/+=";

struct sheet {
  unsigned char *bits;
  unsigned width, height, bpl;
};

/* Hundredths of a millimetre to pixels, rounded to nearest: 2540 hundredths
 * of a millimetre to the inch. The same conversion `pwg-probe-input.c` uses. */
static unsigned px(unsigned hmm, unsigned dpi) { return (hmm * dpi + 1270u) / 2540u; }

/* The pixel column `band_placement` drops up to: `hard_margin_bytes` in
 * crates/spl2-core/src/geometry.rs -- ceil to a pixel, then up to a byte. */
static unsigned drop_columns(unsigned xdpi) {
  unsigned p = (unsigned)ceil(MARGIN_PT * (double)xdpi / 72.0);
  return (p + 7u) & ~7u;
}

/* The scanline count the driver drops from the top: `hard_margin_lines`,
 * which rounds to the nearest scanline rather than up (open question Q-13). */
static unsigned drop_lines(unsigned ydpi) {
  return (unsigned)(MARGIN_PT * (double)ydpi / 72.0 + 0.5);
}

static void dot(struct sheet *s, unsigned x, unsigned y) {
  if (x >= s->width || y >= s->height) return;
  s->bits[(size_t)y * s->bpl + x / 8u] |= (unsigned char)(0x80u >> (x % 8u));
}

static void rect(struct sheet *s, long x, long y, long w, long h) {
  for (long j = y; j < y + h; j++) {
    if (j < 0) continue;
    for (long i = x; i < x + w; i++) {
      if (i < 0) continue;
      dot(s, (unsigned)i, (unsigned)j);
    }
  }
}

static void glyph(struct sheet *s, char c, long x, long y, unsigned scale) {
  const char *found = strchr(FONT_CHARS, c);
  size_t index = found ? (size_t)(found - FONT_CHARS) : 0;
  for (int row = 0; row < GLYPH_H; row++) {
    unsigned char bits = FONT[index][row];
    for (int col = 0; col < GLYPH_W; col++) {
      if (bits & (0x10u >> col))
        rect(s, x + (long)((unsigned)col * scale), y + (long)((unsigned)row * scale),
             (long)scale, (long)scale);
    }
  }
}

static void caption(struct sheet *s, const char *text, long x, long y, unsigned scale) {
  for (const char *c = text; *c; c++) {
    char upper = (*c >= 'a' && *c <= 'z') ? (char)(*c - 32) : *c;
    if (upper == '_') upper = '-';
    glyph(s, upper, x, y, scale);
    x += (long)((unsigned)(GLYPH_W + 1) * scale);
  }
}

/* Where a whole-millimetre PREDICTED PHYSICAL distance from an edge lands in
 * the incoming raster. `drop` is the pixel the engine's origin corresponds to
 * on this axis, and MARGIN_HMM is the physical distance that origin is
 * predicted to sit at. Returns a signed value: negative means the tick would
 * fall off the sheet entirely. */
static long raster_for_physical(unsigned phys_hmm, unsigned drop, unsigned dpi) {
  double offset = ((double)phys_hmm - (double)MARGIN_HMM) * (double)dpi / 2540.0;
  return (long)drop + (long)llround(offset);
}

/* One ruler's identity: which paper edge it measures from. */
struct ruler {
  const char *name;
  unsigned axis;     /* 0 = the tick position varies in x, 1 = in y */
  unsigned from_far; /* measured from the right or bottom paper edge */
};

int main(int argc, char **argv) {
  if (argc != 5) {
    fprintf(stderr, "usage: %s out.pwg pwg-media xdpi ydpi\n", argv[0]);
    return 2;
  }
  pwg_media_t *media = pwgMediaForPWG(argv[2]);
  int xdpi_in = atoi(argv[3]), ydpi_in = atoi(argv[4]);
  if (!media || xdpi_in <= 0 || ydpi_in <= 0) return 2;
  unsigned xdpi = (unsigned)xdpi_in, ydpi = (unsigned)ydpi_in;

  cups_page_header2_t h;
  memset(&h, 0, sizeof(h));
  if (!cupsRasterInitPWGHeader(&h, media, "black_1", xdpi_in, ydpi_in, "one-sided", "normal"))
    return 3;

  struct sheet s;
  s.width = h.cupsWidth;
  s.height = h.cupsHeight;
  s.bpl = h.cupsBytesPerLine;
  s.bits = calloc(s.height, s.bpl);
  if (!s.bits) return 6;

  unsigned sheet_w_hmm = (unsigned)media->width, sheet_h_hmm = (unsigned)media->length;
  unsigned drop_x = drop_columns(xdpi), skip_y = drop_lines(ydpi);
  if (s.height <= 2u * skip_y || s.width <= drop_x) {
    fprintf(stderr, "%s at %ux%u dpi is smaller than its margins\n", argv[2], xdpi, ydpi);
    return 3;
  }
  /* What the driver keeps: crates/ml216x-printer-app/src/driver.rs. */
  unsigned printable_h = s.height - 2u * skip_y;
  unsigned last_row = skip_y + printable_h - 1u;
  unsigned last_col = s.width - 1u;

  unsigned tick_thick_x = px(TICK_THICK_HMM, xdpi) < 2u ? 2u : px(TICK_THICK_HMM, xdpi);
  unsigned tick_thick_y = px(TICK_THICK_HMM, ydpi) < 2u ? 2u : px(TICK_THICK_HMM, ydpi);

  /* Ticks stop at a quarter of the shorter printable side, so a small medium
   * or an envelope gets a shorter ruler instead of one running off the page. */
  unsigned limit_x = (sheet_w_hmm - 2u * MARGIN_HMM) / 4u;
  unsigned limit_y = (sheet_h_hmm - 2u * MARGIN_HMM) / 4u;
  unsigned range_x = limit_x < TICK_MAX_HMM ? limit_x : TICK_MAX_HMM;
  unsigned range_y = limit_y < TICK_MAX_HMM ? limit_y : TICK_MAX_HMM;

  /* Everything below both draws and DECLARES what it drew. The harness in
   * `scripts/g1-probe.py` is a pure consumer of this JSON: it re-derives no
   * position of its own, so the only thing it can disagree with is the
   * driver's transform, which is what it is there to check. */
  struct tick {
    unsigned mm;
    long at;      /* raster coordinate along the measured axis */
    unsigned length_hmm;
    int in_stream;
    int on_paper;
  };
  struct built {
    const char *name;
    unsigned axis;     /* 1 = the ticks march down the page, 0 = across it */
    long scan;         /* a raster line crossing every tick of this ruler */
    long window[2];    /* and the raster range in which only they appear */
    unsigned thickness;
    unsigned count;
    struct tick ticks[64];
  } built[4];

  static const struct ruler RULERS[4] = {
      {"top", 1, 0}, {"bottom", 1, 1}, {"left", 0, 0}, {"right", 0, 1}};
  for (unsigned r = 0; r < 4; r++) {
    const struct ruler *ruler = &RULERS[r];
    struct built *b = &built[r];
    b->name = ruler->name;
    b->axis = ruler->axis;
    b->thickness = ruler->axis ? tick_thick_y : tick_thick_x;
    b->count = 0;

    unsigned range = ruler->axis ? range_y : range_x;
    unsigned dpi = ruler->axis ? ydpi : xdpi;
    unsigned drop = ruler->axis ? skip_y : drop_x;
    unsigned sheet_hmm = ruler->axis ? sheet_h_hmm : sheet_w_hmm;
    /* The four rulers get separate lanes across the sheet, and the scan line
     * sits 2 mm into the ticks so even a 4 mm minor tick is crossed. */
    unsigned lane_hmm = (ruler->axis ? sheet_w_hmm : sheet_h_hmm) / 5u * (r + 1u);
    long lane = ruler->axis ? raster_for_physical(lane_hmm, drop_x, xdpi)
                            : raster_for_physical(lane_hmm, skip_y, ydpi);
    b->scan = lane + (long)px(200u, ruler->axis ? xdpi : ydpi);

    long lowest = 0, highest = 0;
    for (unsigned d = TICK_STEP_HMM; d <= range; d += TICK_STEP_HMM) {
      unsigned mm = d / 100u;
      unsigned length_hmm = (mm % 10u == 0u)   ? TICK_MAJOR_HMM
                            : (mm % 5u == 0u)  ? TICK_MEDIUM_HMM
                                               : TICK_MINOR_HMM;
      unsigned phys = ruler->from_far ? sheet_hmm - d : d;
      long at = raster_for_physical(phys, drop, dpi);
      /* Two predictions, and they part company on the right-hand edge.
       * `in_stream` is what this driver emits: it drops the left margin and
       * crops top and bottom, but nothing trims the right, so a tick 1 mm from
       * the right paper edge is encoded and simply never printed. `on_paper`
       * is where the engine can lay toner: inside the margin on every edge.
       * The harness checks the first, the ruler checks the second. */
      int in_stream;
      if (ruler->axis) {
        rect(&s, lane, at, (long)px(length_hmm, xdpi), (long)tick_thick_y);
        in_stream = at >= (long)skip_y && at <= (long)last_row;
      } else {
        rect(&s, at, lane, (long)tick_thick_x, (long)px(length_hmm, ydpi));
        in_stream = at >= (long)drop_x && at <= (long)last_col;
      }
      /* Every tenth tick is numbered, so the measurement does not depend on
       * counting ticks by eye. The label sits a millimetre past the end of the
       * major tick, which keeps it off this ruler's scan line: that line
       * crosses the ticks 2 mm in. */
      if (mm % 10u == 0u) {
        char label[16];
        snprintf(label, sizeof(label), "%u", mm);
        unsigned lscale = px(35u, ydpi) < 2u ? 2u : px(35u, ydpi);
        if (ruler->axis)
          caption(&s, label, lane + (long)px(TICK_MAJOR_HMM + 100u, xdpi),
                  at - (long)((unsigned)GLYPH_H * lscale) / 2, lscale);
        else
          caption(&s, label, at - (long)((unsigned)GLYPH_W * lscale) / 2,
                  lane + (long)px(TICK_MAJOR_HMM + 100u, ydpi), lscale);
      }
      struct tick *t = &b->ticks[b->count++];
      t->mm = mm;
      t->at = at;
      t->length_hmm = length_hmm;
      t->in_stream = in_stream;
      t->on_paper = in_stream && d >= MARGIN_HMM;
      if (b->count == 1 || at < lowest) lowest = at;
      if (b->count == 1 || at > highest) highest = at;
    }
    /* A window wide enough to catch a tick that moved, narrow enough to hold
     * nothing else the page draws. */
    b->window[0] = lowest - 3 * (long)b->thickness;
    b->window[1] = highest + 3 * (long)b->thickness;
  }

  /* Corner brackets, at the corners of the PREDICTED PRINTABLE AREA rather
   * than of the raster. Left and top are the first pixel this driver emits, so
   * measuring them measures the margin directly. The right-hand column is the
   * far margin's predicted position: the raster runs on to the paper edge but
   * the engine stops, so a bracket at `last_col` would be clipped and would
   * measure nothing. The bottom is `last_row`, the crop the driver applies. */
  unsigned arm_x = px(BRACKET_ARM_HMM, xdpi), arm_y = px(BRACKET_ARM_HMM, ydpi);
  unsigned th_x = px(BRACKET_THICK_HMM, xdpi), th_y = px(BRACKET_THICK_HMM, ydpi);
  long right_col = raster_for_physical(sheet_w_hmm - MARGIN_HMM, drop_x, xdpi);
  if (right_col > (long)last_col) right_col = (long)last_col;
  struct corner {
    const char *name;
    unsigned x, y;
    int dx, dy;
  } corners[4] = {
      {"top-left", drop_x, skip_y, 1, 1},
      {"top-right", (unsigned)right_col, skip_y, -1, 1},
      {"bottom-left", drop_x, last_row, 1, -1},
      {"bottom-right", (unsigned)right_col, last_row, -1, -1},
  };
  for (unsigned c = 0; c < 4; c++) {
    struct corner *k = &corners[c];
    long x0 = k->dx > 0 ? (long)k->x : (long)k->x - (long)arm_x + 1;
    long y0 = k->dy > 0 ? (long)k->y : (long)k->y - (long)th_y + 1;
    rect(&s, x0, y0, (long)arm_x, (long)th_y);
    x0 = k->dx > 0 ? (long)k->x : (long)k->x - (long)th_x + 1;
    y0 = k->dy > 0 ? (long)k->y : (long)k->y - (long)arm_y + 1;
    rect(&s, x0, y0, (long)th_x, (long)arm_y);
  }

  /* A calibration cross of an exact whole-centimetre span, so a
   * resolution-axis mix-up (risk R-4) shows up as a wrong length rather than
   * as an offset a margin error could equally explain. Each bar carries an end
   * tick, and the declared extent runs from the outer edge of one end tick to
   * the outer edge of the other. */
  unsigned span_choice[3] = {10000u, 5000u, 2500u};
  unsigned span_hmm = 0;
  for (unsigned i = 0; i < 3; i++) {
    if (span_choice[i] + 2u * MARGIN_HMM + 1000u <= sheet_w_hmm &&
        span_choice[i] + 2u * MARGIN_HMM + 1000u <= sheet_h_hmm) {
      span_hmm = span_choice[i];
      break;
    }
  }
  long h_scan = 0, h_extent[2] = {0, 0}, v_scan = 0, v_extent[2] = {0, 0};
  if (span_hmm) {
    long mid_x = (long)px(sheet_w_hmm / 2u, xdpi), mid_y = (long)px(sheet_h_hmm / 2u, ydpi);
    long len_x = (long)px(span_hmm, xdpi), len_y = (long)px(span_hmm, ydpi);
    long ear_x = (long)px(600u, xdpi), ear_y = (long)px(600u, ydpi);
    rect(&s, mid_x - len_x / 2, mid_y, len_x, (long)tick_thick_y);
    rect(&s, mid_x - len_x / 2, mid_y - ear_y / 2, (long)tick_thick_x, ear_y);
    rect(&s, mid_x + len_x / 2, mid_y - ear_y / 2, (long)tick_thick_x, ear_y);
    rect(&s, mid_x, mid_y - len_y / 2, (long)tick_thick_x, len_y);
    rect(&s, mid_x - ear_x / 2, mid_y - len_y / 2, ear_x, (long)tick_thick_y);
    rect(&s, mid_x - ear_x / 2, mid_y + len_y / 2, ear_x, (long)tick_thick_y);
    h_scan = mid_y;
    h_extent[0] = mid_x - len_x / 2;
    h_extent[1] = mid_x + len_x / 2 + (long)tick_thick_x - 1;
    v_scan = mid_x;
    v_extent[0] = mid_y - len_y / 2;
    v_extent[1] = mid_y + len_y / 2 + (long)tick_thick_y - 1;
  }

  /* The page identifies itself: a stack of test prints is otherwise a stack of
   * indistinguishable rulers. It sits below the top ruler, which reaches 25 mm
   * down the sheet, and above the calibration cross at the centre. */
  char line1[128], line2[128];
  snprintf(line1, sizeof(line1), "G1 %s %uX%u", argv[2], xdpi, ydpi);
  for (char *c = line1; *c; c++) {
    if (*c >= 'a' && *c <= 'z') *c = (char)(*c - 32);
    if (*c == '_') *c = '-';
  }
  snprintf(line2, sizeof(line2), "MARGIN %.1fPT=%u.%02uMM SPAN %uMM", MARGIN_PT,
           MARGIN_HMM / 100u, MARGIN_HMM % 100u, span_hmm / 100u);
  unsigned scale = px(43u, ydpi) < 2u ? 2u : px(43u, ydpi);
  long text_x = raster_for_physical(MARGIN_HMM + 1500u, drop_x, xdpi);
  long text_y = raster_for_physical(MARGIN_HMM + 4000u, skip_y, ydpi);
  caption(&s, line1, text_x, text_y, scale);
  caption(&s, line2, text_x, text_y + (long)((unsigned)(GLYPH_H + 2) * scale), scale);

  printf("{\n");
  printf("  \"media\": \"%s\", \"resolution\": [%u, %u],\n", argv[2], xdpi, ydpi);
  printf("  \"sheet_hmm\": [%u, %u],\n", sheet_w_hmm, sheet_h_hmm);
  printf("  \"raster\": {\"width\": %u, \"height\": %u, \"bytes_per_line\": %u},\n",
         s.width, s.height, s.bpl);
  printf("  \"margin_pt\": %.2f, \"margin_hmm\": %u,\n", MARGIN_PT, MARGIN_HMM);
  printf("  \"drop_columns\": %u, \"drop_lines\": %u,\n", drop_x, skip_y);
  printf("  \"printable_lines\": %u, \"last_row\": %u, \"last_col\": %u,\n",
         printable_h, last_row, last_col);
  /* The byte-alignment shift, reported rather than buried in the drawing: how
   * much further left the sheet content is predicted to sit than its nominal
   * position, because the horizontal margin rounds up to a whole byte. */
  printf("  \"horizontal_alignment_shift_hmm\": %.1f,\n",
         (double)drop_x * 2540.0 / (double)xdpi - (double)MARGIN_HMM);
  printf("  \"rulers\": [\n");
  for (unsigned r = 0; r < 4; r++) {
    struct built *b = &built[r];
    printf("    {\"name\": \"%s\", \"axis\": \"%s\", \"scan\": %ld, "
           "\"window\": [%ld, %ld], \"thickness\": %u, \"ticks\": [\n",
           b->name, b->axis ? "y" : "x", b->scan, b->window[0], b->window[1],
           b->thickness);
    for (unsigned t = 0; t < b->count; t++) {
      struct tick *k = &b->ticks[t];
      printf("      {\"mm\": %u, \"raster\": %ld, \"length_hmm\": %u, "
             "\"in_stream\": %s, \"on_paper\": %s}%s\n",
             k->mm, k->at, k->length_hmm, k->in_stream ? "true" : "false",
             k->on_paper ? "true" : "false", t + 1 == b->count ? "" : ",");
    }
    printf("    ]}%s\n", r == 3 ? "" : ",");
  }
  printf("  ],\n  \"brackets\": [\n");
  for (unsigned c = 0; c < 4; c++) {
    struct corner *k = &corners[c];
    double phys_x = ((double)k->x - (double)drop_x) * 2540.0 / (double)xdpi + MARGIN_HMM;
    double phys_y = ((double)k->y - (double)skip_y) * 2540.0 / (double)ydpi + MARGIN_HMM;
    printf("    {\"corner\": \"%s\", \"raster\": [%u, %u], \"arm_hmm\": %u, "
           "\"predicted_hmm\": [%.1f, %.1f]}%s\n",
           k->name, k->x, k->y, BRACKET_ARM_HMM,
           k->dx > 0 ? phys_x : (double)sheet_w_hmm - phys_x,
           k->dy > 0 ? phys_y : (double)sheet_h_hmm - phys_y, c == 3 ? "" : ",");
  }
  printf("  ],\n");
  printf("  \"calibration\": {\"span_hmm\": %u, "
         "\"horizontal\": {\"scan\": %ld, \"extent\": [%ld, %ld]}, "
         "\"vertical\": {\"scan\": %ld, \"extent\": [%ld, %ld]}},\n",
         span_hmm, h_scan, h_extent[0], h_extent[1], v_scan, v_extent[0], v_extent[1]);
  printf("  \"caption\": [\"%s\", \"%s\"], \"caption_scale\": %u\n}\n", line1, line2, scale);

  int fd = open(argv[1], O_WRONLY | O_CREAT | O_TRUNC, 0600);
  if (fd < 0) return 4;
  cups_raster_t *raster = cupsRasterOpen(fd, CUPS_RASTER_WRITE_PWG);
  if (!raster || !cupsRasterWriteHeader2(raster, &h)) return 5;
  for (unsigned y = 0; y < s.height; y++)
    if (cupsRasterWritePixels(raster, s.bits + (size_t)y * s.bpl, s.bpl) != s.bpl) return 7;
  cupsRasterClose(raster);
  close(fd);
  free(s.bits);
  return 0;
}
