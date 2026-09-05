/*
 * SPDX-License-Identifier: Apache-2.0 OR MIT
 *
 * Layout probe for pappl-sys.
 *
 * The bindings in this crate are hand written, so nothing checks our
 * transcription of the C headers except a program compiled against those same
 * headers. This is that program: it prints the size and alignment of every
 * type we declare, the byte offset and size of every field we name, and the
 * integer value of every enum constant we hardcode.
 *
 * build.rs compiles and runs it with the flags pkg-config reports for PAPPL,
 * and tests/layout.rs asserts the Rust side against the output. A libpappl
 * upgrade that moves a field therefore breaks the build instead of silently
 * corrupting memory.
 *
 * Output format, one record per line:
 *   T <name> <size> <align>
 *   F <type>.<field> <offset> <size>
 *   E <name> <value>
 *   V <name> <value>
 */

#include <pappl/pappl.h>
#include <stddef.h>
#include <stdio.h>

#define TYPE(t)      printf("T %s %zu %zu\n", #t, sizeof(t), _Alignof(t))
#define FIELD(t, f)  printf("F %s.%s %zu %zu\n", #t, #f, offsetof(t, f), sizeof(((t *)0)->f))
#define ENUM(e)      printf("E %s %lld\n", #e, (long long)(e))

int main(void)
{
  /*
   * The CUPS version whose headers defined cups_page_header2_t for this
   * build. That struct is embedded by value at the start of
   * pappl_pr_options_t, so its layout is part of PAPPL's ABI even though it
   * belongs to CUPS. See risk R-6 in docs/MIGRATION-PLAN.md.
   */
  printf("V CUPS_VERSION_MAJOR %d\n", CUPS_VERSION_MAJOR);
  printf("V CUPS_VERSION_MINOR %d\n", CUPS_VERSION_MINOR);
  printf("V CUPS_VERSION_PATCH %d\n", CUPS_VERSION_PATCH);

  /* ---- types we declare ------------------------------------------------ */
  TYPE(pappl_pr_driver_data_t);
  TYPE(pappl_pr_options_t);
  TYPE(pappl_media_col_t);
  TYPE(pappl_icon_t);
  TYPE(pappl_pr_driver_t);
  TYPE(pappl_supply_t);
  TYPE(pappl_dither_t);
  TYPE(cups_page_header2_t);
  ENUM(CUPS_CSPACE_K);
  ENUM(CUPS_ORDER_CHUNKED);
  ENUM(IPP_ORIENT_NONE);
  ENUM(IPP_QUALITY_NORMAL);

  FIELD(cups_page_header2_t, MediaClass);
  FIELD(cups_page_header2_t, MediaColor);
  FIELD(cups_page_header2_t, MediaType);
  FIELD(cups_page_header2_t, OutputType);
  FIELD(cups_page_header2_t, AdvanceDistance);
  FIELD(cups_page_header2_t, AdvanceMedia);
  FIELD(cups_page_header2_t, Collate);
  FIELD(cups_page_header2_t, CutMedia);
  FIELD(cups_page_header2_t, Duplex);
  FIELD(cups_page_header2_t, HWResolution);
  FIELD(cups_page_header2_t, ImagingBoundingBox);
  FIELD(cups_page_header2_t, InsertSheet);
  FIELD(cups_page_header2_t, Jog);
  FIELD(cups_page_header2_t, LeadingEdge);
  FIELD(cups_page_header2_t, Margins);
  FIELD(cups_page_header2_t, ManualFeed);
  FIELD(cups_page_header2_t, MediaPosition);
  FIELD(cups_page_header2_t, MediaWeight);
  FIELD(cups_page_header2_t, MirrorPrint);
  FIELD(cups_page_header2_t, NegativePrint);
  FIELD(cups_page_header2_t, NumCopies);
  FIELD(cups_page_header2_t, Orientation);
  FIELD(cups_page_header2_t, OutputFaceUp);
  FIELD(cups_page_header2_t, PageSize);
  FIELD(cups_page_header2_t, Separations);
  FIELD(cups_page_header2_t, TraySwitch);
  FIELD(cups_page_header2_t, Tumble);
  FIELD(cups_page_header2_t, cupsWidth);
  FIELD(cups_page_header2_t, cupsHeight);
  FIELD(cups_page_header2_t, cupsMediaType);
  FIELD(cups_page_header2_t, cupsBitsPerColor);
  FIELD(cups_page_header2_t, cupsBitsPerPixel);
  FIELD(cups_page_header2_t, cupsBytesPerLine);
  FIELD(cups_page_header2_t, cupsColorOrder);
  FIELD(cups_page_header2_t, cupsColorSpace);
  FIELD(cups_page_header2_t, cupsCompression);
  FIELD(cups_page_header2_t, cupsRowCount);
  FIELD(cups_page_header2_t, cupsRowFeed);
  FIELD(cups_page_header2_t, cupsRowStep);
  FIELD(cups_page_header2_t, cupsNumColors);
  FIELD(cups_page_header2_t, cupsBorderlessScalingFactor);
  FIELD(cups_page_header2_t, cupsPageSize);
  FIELD(cups_page_header2_t, cupsImagingBBox);
  FIELD(cups_page_header2_t, cupsInteger);
  FIELD(cups_page_header2_t, cupsReal);
  FIELD(cups_page_header2_t, cupsString);
  FIELD(cups_page_header2_t, cupsMarkerType);
  FIELD(cups_page_header2_t, cupsRenderingIntent);
  FIELD(cups_page_header2_t, cupsPageSizeName);


  /* ---- limits that size the arrays inside them ------------------------- */
  ENUM(PAPPL_MAX_BIN);
  ENUM(PAPPL_MAX_MEDIA);
  ENUM(PAPPL_MAX_RESOLUTION);
  ENUM(PAPPL_MAX_SOURCE);
  ENUM(PAPPL_MAX_TYPE);
  ENUM(PAPPL_MAX_VENDOR);

  /* ---- pappl_icon_t ---------------------------------------------------- */
  FIELD(pappl_icon_t, filename);
  FIELD(pappl_icon_t, data);
  FIELD(pappl_icon_t, datalen);

  /* ---- pappl_media_col_t ----------------------------------------------- */
  FIELD(pappl_media_col_t, bottom_margin);
  FIELD(pappl_media_col_t, left_margin);
  FIELD(pappl_media_col_t, left_offset);
  FIELD(pappl_media_col_t, right_margin);
  FIELD(pappl_media_col_t, size_width);
  FIELD(pappl_media_col_t, size_length);
  FIELD(pappl_media_col_t, size_name);
  FIELD(pappl_media_col_t, source);
  FIELD(pappl_media_col_t, top_margin);
  FIELD(pappl_media_col_t, top_offset);
  FIELD(pappl_media_col_t, tracking);
  FIELD(pappl_media_col_t, type);

  /* ---- pappl_pr_driver_t ----------------------------------------------- */
  FIELD(pappl_pr_driver_t, name);
  FIELD(pappl_pr_driver_t, description);
  FIELD(pappl_pr_driver_t, device_id);
  FIELD(pappl_pr_driver_t, extension);

  /* ---- pappl_supply_t -------------------------------------------------- */
  FIELD(pappl_supply_t, color);
  FIELD(pappl_supply_t, description);
  FIELD(pappl_supply_t, is_consumed);
  FIELD(pappl_supply_t, level);
  FIELD(pappl_supply_t, type);

  /* ---- pappl_pr_options_t ---------------------------------------------- */
  FIELD(pappl_pr_options_t, header);
  FIELD(pappl_pr_options_t, num_pages);
  FIELD(pappl_pr_options_t, first_page);
  FIELD(pappl_pr_options_t, last_page);
  FIELD(pappl_pr_options_t, dither);
  FIELD(pappl_pr_options_t, copies);
  FIELD(pappl_pr_options_t, finishings);
  FIELD(pappl_pr_options_t, media);
  FIELD(pappl_pr_options_t, orientation_requested);
  FIELD(pappl_pr_options_t, output_bin);
  FIELD(pappl_pr_options_t, print_color_mode);
  FIELD(pappl_pr_options_t, print_content_optimize);
  FIELD(pappl_pr_options_t, print_darkness);
  FIELD(pappl_pr_options_t, darkness_configured);
  FIELD(pappl_pr_options_t, print_quality);
  FIELD(pappl_pr_options_t, print_scaling);
  FIELD(pappl_pr_options_t, print_speed);
  FIELD(pappl_pr_options_t, printer_resolution);
  FIELD(pappl_pr_options_t, sides);
  FIELD(pappl_pr_options_t, num_vendor);
  FIELD(pappl_pr_options_t, vendor);

  /* ---- pappl_pr_driver_data_t ------------------------------------------ */
  FIELD(pappl_pr_driver_data_t, extension);
  FIELD(pappl_pr_driver_data_t, delete_cb);
  FIELD(pappl_pr_driver_data_t, identify_cb);
  FIELD(pappl_pr_driver_data_t, printfile_cb);
  FIELD(pappl_pr_driver_data_t, rendjob_cb);
  FIELD(pappl_pr_driver_data_t, rendpage_cb);
  FIELD(pappl_pr_driver_data_t, rstartjob_cb);
  FIELD(pappl_pr_driver_data_t, rstartpage_cb);
  FIELD(pappl_pr_driver_data_t, rwriteline_cb);
  FIELD(pappl_pr_driver_data_t, status_cb);
  FIELD(pappl_pr_driver_data_t, testpage_cb);
  FIELD(pappl_pr_driver_data_t, gdither);
  FIELD(pappl_pr_driver_data_t, pdither);
  FIELD(pappl_pr_driver_data_t, format);
  FIELD(pappl_pr_driver_data_t, make_and_model);
  FIELD(pappl_pr_driver_data_t, ppm);
  FIELD(pappl_pr_driver_data_t, ppm_color);
  FIELD(pappl_pr_driver_data_t, icons);
  FIELD(pappl_pr_driver_data_t, kind);
  FIELD(pappl_pr_driver_data_t, has_supplies);
  FIELD(pappl_pr_driver_data_t, input_face_up);
  FIELD(pappl_pr_driver_data_t, output_face_up);
  FIELD(pappl_pr_driver_data_t, orient_default);
  FIELD(pappl_pr_driver_data_t, color_supported);
  FIELD(pappl_pr_driver_data_t, color_default);
  FIELD(pappl_pr_driver_data_t, content_default);
  FIELD(pappl_pr_driver_data_t, quality_default);
  FIELD(pappl_pr_driver_data_t, scaling_default);
  FIELD(pappl_pr_driver_data_t, raster_types);
  FIELD(pappl_pr_driver_data_t, force_raster_type);
  FIELD(pappl_pr_driver_data_t, duplex);
  FIELD(pappl_pr_driver_data_t, sides_supported);
  FIELD(pappl_pr_driver_data_t, sides_default);
  FIELD(pappl_pr_driver_data_t, finishings);
  FIELD(pappl_pr_driver_data_t, num_resolution);
  FIELD(pappl_pr_driver_data_t, x_resolution);
  FIELD(pappl_pr_driver_data_t, y_resolution);
  FIELD(pappl_pr_driver_data_t, x_default);
  FIELD(pappl_pr_driver_data_t, y_default);
  FIELD(pappl_pr_driver_data_t, borderless);
  FIELD(pappl_pr_driver_data_t, left_right);
  FIELD(pappl_pr_driver_data_t, bottom_top);
  FIELD(pappl_pr_driver_data_t, num_media);
  FIELD(pappl_pr_driver_data_t, media);
  FIELD(pappl_pr_driver_data_t, media_default);
  FIELD(pappl_pr_driver_data_t, media_ready);
  FIELD(pappl_pr_driver_data_t, num_source);
  FIELD(pappl_pr_driver_data_t, source);
  FIELD(pappl_pr_driver_data_t, left_offset_supported);
  FIELD(pappl_pr_driver_data_t, top_offset_supported);
  FIELD(pappl_pr_driver_data_t, tracking_supported);
  FIELD(pappl_pr_driver_data_t, num_type);
  FIELD(pappl_pr_driver_data_t, type);
  FIELD(pappl_pr_driver_data_t, num_bin);
  FIELD(pappl_pr_driver_data_t, bin);
  FIELD(pappl_pr_driver_data_t, bin_default);
  FIELD(pappl_pr_driver_data_t, mode_configured);
  FIELD(pappl_pr_driver_data_t, mode_supported);
  FIELD(pappl_pr_driver_data_t, tear_offset_configured);
  FIELD(pappl_pr_driver_data_t, tear_offset_supported);
  FIELD(pappl_pr_driver_data_t, speed_supported);
  FIELD(pappl_pr_driver_data_t, speed_default);
  FIELD(pappl_pr_driver_data_t, darkness_default);
  FIELD(pappl_pr_driver_data_t, darkness_configured);
  FIELD(pappl_pr_driver_data_t, darkness_supported);
  FIELD(pappl_pr_driver_data_t, identify_default);
  FIELD(pappl_pr_driver_data_t, identify_supported);
  FIELD(pappl_pr_driver_data_t, num_features);
  FIELD(pappl_pr_driver_data_t, features);
  FIELD(pappl_pr_driver_data_t, num_vendor);
  FIELD(pappl_pr_driver_data_t, vendor);

  /* ---- enum constants we hardcode -------------------------------------- */
  ENUM(PAPPL_LOGLEVEL_UNSPEC);
  ENUM(PAPPL_LOGLEVEL_DEBUG);
  ENUM(PAPPL_LOGLEVEL_INFO);
  ENUM(PAPPL_LOGLEVEL_WARN);
  ENUM(PAPPL_LOGLEVEL_ERROR);
  ENUM(PAPPL_LOGLEVEL_FATAL);

  ENUM(PAPPL_PWG_RASTER_TYPE_NONE);
  ENUM(PAPPL_PWG_RASTER_TYPE_BLACK_1);
  ENUM(PAPPL_PWG_RASTER_TYPE_BLACK_8);
  ENUM(PAPPL_PWG_RASTER_TYPE_SGRAY_8);

  ENUM(PAPPL_COLOR_MODE_AUTO);
  ENUM(PAPPL_COLOR_MODE_AUTO_MONOCHROME);
  ENUM(PAPPL_COLOR_MODE_BI_LEVEL);
  ENUM(PAPPL_COLOR_MODE_MONOCHROME);

  ENUM(PAPPL_CONTENT_AUTO);
  ENUM(PAPPL_CONTENT_GRAPHIC);
  ENUM(PAPPL_CONTENT_PHOTO);
  ENUM(PAPPL_CONTENT_TEXT);
  ENUM(PAPPL_CONTENT_TEXT_AND_GRAPHIC);

  ENUM(PAPPL_DUPLEX_NONE);
  ENUM(PAPPL_DUPLEX_NORMAL);

  ENUM(PAPPL_SIDES_ONE_SIDED);
  ENUM(PAPPL_SIDES_TWO_SIDED_LONG_EDGE);
  ENUM(PAPPL_SIDES_TWO_SIDED_SHORT_EDGE);

  ENUM(PAPPL_SCALING_AUTO);
  ENUM(PAPPL_SCALING_AUTO_FIT);
  ENUM(PAPPL_SCALING_FILL);
  ENUM(PAPPL_SCALING_FIT);
  ENUM(PAPPL_SCALING_NONE);

  ENUM(PAPPL_KIND_DOCUMENT);
  ENUM(PAPPL_KIND_ENVELOPE);

  ENUM(PAPPL_IDENTIFY_ACTIONS_NONE);
  ENUM(PAPPL_IDENTIFY_ACTIONS_DISPLAY);
  ENUM(PAPPL_IDENTIFY_ACTIONS_SOUND);

  ENUM(PAPPL_MEDIA_TRACKING_CONTINUOUS);
  ENUM(PAPPL_MEDIA_TRACKING_GAP);
  ENUM(PAPPL_MEDIA_TRACKING_MARK);
  ENUM(PAPPL_MEDIA_TRACKING_WEB);

  ENUM(PAPPL_SOPTIONS_NONE);
  ENUM(PAPPL_SOPTIONS_MULTI_QUEUE);
  ENUM(PAPPL_SOPTIONS_RAW_SOCKET);
  ENUM(PAPPL_SOPTIONS_USB_PRINTER);
  ENUM(PAPPL_SOPTIONS_WEB_INTERFACE);
  ENUM(PAPPL_SOPTIONS_WEB_LOG);
  ENUM(PAPPL_SOPTIONS_WEB_NETWORK);
  ENUM(PAPPL_SOPTIONS_WEB_REMOTE);
  ENUM(PAPPL_SOPTIONS_WEB_SECURITY);
  ENUM(PAPPL_SOPTIONS_WEB_TLS);
  ENUM(PAPPL_SOPTIONS_NO_TLS);

  ENUM(PAPPL_DEVTYPE_FILE);
  ENUM(PAPPL_DEVTYPE_USB);
  ENUM(PAPPL_DEVTYPE_SOCKET);
  ENUM(PAPPL_DEVTYPE_DNS_SD);
  ENUM(PAPPL_DEVTYPE_LOCAL);
  ENUM(PAPPL_DEVTYPE_NETWORK);
  ENUM(PAPPL_DEVTYPE_ALL);

  ENUM(PAPPL_PREASON_NONE);
  ENUM(PAPPL_PREASON_OTHER);
  ENUM(PAPPL_PREASON_COVER_OPEN);
  ENUM(PAPPL_PREASON_MEDIA_EMPTY);
  ENUM(PAPPL_PREASON_MEDIA_JAM);
  ENUM(PAPPL_PREASON_MEDIA_LOW);
  ENUM(PAPPL_PREASON_MEDIA_NEEDED);
  ENUM(PAPPL_PREASON_TONER_EMPTY);
  ENUM(PAPPL_PREASON_TONER_LOW);
  ENUM(PAPPL_PREASON_DOOR_OPEN);

  ENUM(PAPPL_JREASON_NONE);
  ENUM(PAPPL_JREASON_ABORTED_BY_SYSTEM);
  ENUM(PAPPL_JREASON_DOCUMENT_FORMAT_ERROR);
  ENUM(PAPPL_JREASON_DOCUMENT_UNPRINTABLE_ERROR);
  ENUM(PAPPL_JREASON_ERRORS_DETECTED);
  ENUM(PAPPL_JREASON_JOB_CANCELED_BY_USER);
  ENUM(PAPPL_JREASON_JOB_COMPLETED_SUCCESSFULLY);
  ENUM(PAPPL_JREASON_JOB_COMPLETED_WITH_ERRORS);
  ENUM(PAPPL_JREASON_WARNINGS_DETECTED);

  return 0;
}
