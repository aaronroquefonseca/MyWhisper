{ pkgs }:
let
  source = pkgs.writeText "svg-probe.c" ''
    #include <gtk/gtk.h>
    #include <stdio.h>
    #include <stdlib.h>
    static unsigned errors, metadata;
    static void svg_error(GtkSvg *svg, GError *error, void *user_data) {
      (void)svg; (void)user_data;
      fprintf(stderr, "SVG diagnostic: %s (code=%d, element=%s)\n", error->message, error->code, gtk_svg_error_get_element(error));
      if (error->domain == GTK_SVG_ERROR && error->code == GTK_SVG_ERROR_IGNORED_ELEMENT &&
          g_strcmp0(error->message, "Ignoring metadata and non-standard elements: <title>") == 0) ++metadata;
      else ++errors;
    }
    int main(int argc, char **argv) {
      if (argc != 3) return 2;
      gtk_init();
      char *source = NULL; gsize length = 0; GError *error = NULL;
      if (!g_file_get_contents(argv[1], &source, &length, &error)) {
        fprintf(stderr, "%s\n", error->message); return 2;
      }
      GBytes *bytes = g_bytes_new_take(source, length);
      GtkSvg *svg = gtk_svg_new();
      g_signal_connect(svg, "error", G_CALLBACK(svg_error), NULL);
      if (atoi(argv[2])) gtk_svg_set_features(svg, gtk_svg_get_features(svg) | GTK_SVG_TRADITIONAL_SYMBOLIC);
      gtk_svg_load_from_bytes(svg, bytes);
      unsigned counts[2] = {0}, color_errors = 0;
      for (unsigned foreground=0; foreground<2; ++foreground) {
        GtkSnapshot *snapshot = gtk_snapshot_new();
        GdkRGBA color = {foreground, foreground, foreground, 1};
        GdkRGBA colors[] = {color, color, color, color};
        gtk_symbolic_paintable_snapshot_symbolic(GTK_SYMBOLIC_PAINTABLE(svg), GDK_SNAPSHOT(snapshot), 32, 32, colors, 4);
        GskRenderNode *node = gtk_snapshot_free_to_node(snapshot);
        cairo_surface_t *surface = cairo_image_surface_create(CAIRO_FORMAT_ARGB32, 32, 32);
        cairo_t *cr = cairo_create(surface);
        if (node) gsk_render_node_draw(node, cr);
        cairo_surface_flush(surface);
        unsigned char *data = cairo_image_surface_get_data(surface);
        for (unsigned y=0; y<32; ++y) for (unsigned x=0; x<32; ++x) {
          unsigned char *pixel = data+y*cairo_image_surface_get_stride(surface)+4*x;
          if (!pixel[3]) continue;
          ++counts[foreground];
          unsigned char expected = foreground ? pixel[3] : 0;
          if (pixel[0] != expected || pixel[1] != expected || pixel[2] != expected) ++color_errors;
        }
        cairo_destroy(cr); cairo_surface_destroy(surface);
        if (node) gsk_render_node_unref(node);
      }
      printf("GTK %u.%u.%u errors=%u color_errors=%u black_pixels=%u white_pixels=%u\n", gtk_get_major_version(), gtk_get_minor_version(), gtk_get_micro_version(), errors, color_errors, counts[0], counts[1]);
      g_object_unref(svg); g_bytes_unref(bytes);
      return errors || color_errors || metadata != 1 || counts[0] < 100 || counts[1] < 100;
    }
  '';
in pkgs.stdenv.mkDerivation {
  pname = "mywhisper-svg-probe"; version = "1";
  dontUnpack = true;
  nativeBuildInputs = [ pkgs.pkg-config ];
  buildInputs = [ pkgs.gtk4 ];
  buildPhase = ''
    $CC -Wall -Wextra -Werror $(pkg-config --cflags gtk4) ${source} -o svg-probe $(pkg-config --libs gtk4)
  '';
  installPhase = ''mkdir -p $out/bin; cp svg-probe $out/bin/'';
}
