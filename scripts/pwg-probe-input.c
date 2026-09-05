/* SPDX-License-Identifier: GPL-2.0-only */
/* Generate real libcups PWG Raster for the P5 callback experiment.
 * Build: cc scripts/pwg-probe-input.c -lcups -o /tmp/pwg-probe-input
 * Usage: pwg-probe-input output.pwg pwg-media xdpi ydpi
 * Four marks at the sheet corners, four at the 441/100 mm inset.
 */
#include <cups/raster.h>
#include <cups/pwg.h>
#include <fcntl.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

int main(int argc, char **argv) {
  if (argc != 5) return 2;
  pwg_media_t *media = pwgMediaForPWG(argv[2]);
  int xdpi = atoi(argv[3]), ydpi = atoi(argv[4]);
  if (!media || xdpi <= 0 || ydpi <= 0) return 2;
  cups_page_header2_t h;
  memset(&h, 0, sizeof(h));
  if (!cupsRasterInitPWGHeader(&h, media, "black_1", xdpi, ydpi, "one-sided", "normal")) return 3;
  int fd = open(argv[1], O_WRONLY | O_CREAT | O_TRUNC, 0600);
  if (fd < 0) return 4;
  cups_raster_t *r = cupsRasterOpen(fd, CUPS_RASTER_WRITE_PWG);
  if (!r || !cupsRasterWriteHeader2(r, &h)) return 5;
  unsigned char *line = calloc(1, h.cupsBytesPerLine);
  if (!line) return 6;
  unsigned mx = (441u * xdpi + 1270u) / 2540u;
  unsigned my = (441u * ydpi + 1270u) / 2540u;
  for (unsigned y = 0; y < h.cupsHeight; y++) {
    memset(line, 0, h.cupsBytesPerLine);
    if (y == 0 || y == h.cupsHeight - 1) {
      line[0] |= 0x80;
      unsigned x = h.cupsWidth - 1;
      line[x / 8] |= 0x80 >> (x % 8);
    }
    if (y == my || y == h.cupsHeight - 1 - my) {
      unsigned x = mx;
      line[x / 8] |= 0x80 >> (x % 8);
      x = h.cupsWidth - 1 - mx;
      line[x / 8] |= 0x80 >> (x % 8);
    }
    if (cupsRasterWritePixels(r, line, h.cupsBytesPerLine) != h.cupsBytesPerLine) return 7;
  }
  printf("{\"width\":%u,\"height\":%u,\"bytes_per_line\":%u,\"inset\":[%u,%u]}\n", h.cupsWidth, h.cupsHeight, h.cupsBytesPerLine, mx, my);
  free(line); cupsRasterClose(r); close(fd);
  return 0;
}
