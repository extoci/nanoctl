#define _POSIX_C_SOURCE 200809L
#include <X11/Xlib.h>
#include <X11/Xutil.h>
#include <X11/extensions/XShm.h>
#include <sys/shm.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>
#include <math.h>

typedef struct { Display *d; XImage *im; XShmSegmentInfo shm; int row, left, right; } Reader;
static int xerror = 0;
static int on_error(Display *d, XErrorEvent *event) { (void)d; (void)event; xerror = 1; return 0; }
static double now(void) { struct timespec t; clock_gettime(CLOCK_MONOTONIC, &t); return t.tv_sec + t.tv_nsec / 1e9; }
static int channel(unsigned long p, unsigned long mask) { if (!mask) return 0; while (!(mask & 1)) { mask >>= 1; p >>= 1; } return (int)((p & mask) * 255 / mask); }
static int marker(Reader *r, int y) {
  int left = -1, right = -1;
  for (int x = 0; x < 1280; x++) {
    unsigned long p = XGetPixel(r->im, x, y);
    int red = channel(p, r->im->red_mask), g = channel(p, r->im->green_mask), b = channel(p, r->im->blue_mask);
    if (red < 80 && g > 150 && b > 150) left = x;
    if (left >= 0 && red > 150 && g < 80 && b > 150) { right = x; break; }
  }
  if (right - left < 144) return -1;
  uint32_t word = 0;
  for (int i = 0; i < 36; i++) {
    int x = left + (int)((right - left) * (i + .5) / 36);
    int red = channel(XGetPixel(r->im, x, y), r->im->red_mask), bit = red > 192 ? 1 : red < 64 ? 0 : -1;
    if (bit < 0 || ((i == 0 || i == 35) && bit != 1) || ((i == 1 || i == 34) && bit != 0)) return -1;
    if (i >= 2 && i < 34) word = (word << 1) | (bit > 0);
  }
  int seq = word >> 16, input = (word >> 8) & 255;
  if ((word & 255) != (((seq >> 8) ^ seq ^ input ^ 0xa5) & 255)) return -1;
  r->left = left; r->right = right; return seq;
}
static int open_reader(Reader *r, const char *display) {
  r->d = XOpenDisplay(display); if (!r->d || !XShmQueryExtension(r->d)) return 0;
  int s = DefaultScreen(r->d); if (DisplayWidth(r->d, s) != 1280 || DisplayHeight(r->d, s) != 720) return 0;
  r->im = XShmCreateImage(r->d, DefaultVisual(r->d, s), DefaultDepth(r->d, s), ZPixmap, NULL, &r->shm, 1280, 720);
  if (!r->im) return 0;
  /* The container X server can have a different UID; these displays contain only benchmark data. */
  r->shm.shmid = shmget(IPC_PRIVATE, r->im->bytes_per_line * 720, IPC_CREAT | 0666); if (r->shm.shmid < 0) return 0;
  r->shm.shmaddr = shmat(r->shm.shmid, NULL, 0); if (r->shm.shmaddr == (void *)-1) { shmctl(r->shm.shmid, IPC_RMID, NULL); return 0; }
  r->im->data = r->shm.shmaddr; r->shm.readOnly = False;
  xerror = 0;
  if (!XShmAttach(r->d, &r->shm)) { shmdt(r->shm.shmaddr); shmctl(r->shm.shmid, IPC_RMID, NULL); return 0; }
  XSync(r->d, False); shmctl(r->shm.shmid, IPC_RMID, NULL);
  if (xerror) { shmdt(r->shm.shmaddr); return 0; }
  if (!XShmGetImage(r->d, DefaultRootWindow(r->d), r->im, 0, 0, AllPlanes)) return 0;
  r->row = -1;
  for (int y = 0; y < 600; y++) if (marker(r, y) >= 0) { r->row = y; break; }
  return r->row >= 0;
}
static int grab(Reader *r, int full) {
  r->im->height = full ? 720 : 1;
  if (!XShmGetImage(r->d, DefaultRootWindow(r->d), r->im, 0, full ? 0 : r->row + 8, AllPlanes)) return -1;
  return marker(r, full ? r->row + 8 : 0);
}
static int save(Reader *r, const char *file, int width, int height, int x, int y) {
  if (x < 0 || y < 0 || x + width > 1280 || y + height > 720 || width < 1 || height < 1) return 0;
  FILE *f = fopen(file, "wb"); if (!f) return 0;
  fprintf(f, "P6\n%d %d\n255\n", width, height);
  for (int row = y; row < y + height; row++) for (int col = x; col < x + width; col++) {
    unsigned long p = XGetPixel(r->im, col, row);
    unsigned char rgb[] = {channel(p, r->im->red_mask), channel(p, r->im->green_mask), channel(p, r->im->blue_mask)};
    if (fwrite(rgb, 1, 3, f) != 3) { fclose(f); return 0; }
  }
  return fclose(f) == 0;
}
static void close_reader(Reader *r) {
  XShmDetach(r->d, &r->shm); XSync(r->d, False); r->im->data = NULL;
  XDestroyImage(r->im); shmdt(r->shm.shmaddr); XCloseDisplay(r->d);
}
int main(int argc, char **argv) {
  XSetErrorHandler(on_error);
  if ((argc != 5 && argc != 6) || atof(argv[3]) <= 0) { fprintf(stderr, "usage: frames SOURCE_DISPLAY CLIENT_DISPLAY SECONDS OUTPUT_PREFIX [SETTLE_MS]\n"); return 2; }
  char *end = NULL;
  double settle = argc == 6 ? strtod(argv[5], &end) / 1000 : 0;
  if (!isfinite(settle) || settle < 0 || settle > 10 || (argc == 6 && (end == argv[5] || *end))) { fprintf(stderr, "Invalid settle delay\n"); return 2; }
  int stride = settle > 0 ? 1 : 15;
  Reader src = {0}, dst = {0};
  if (!open_reader(&src, argv[1]) || !open_reader(&dst, argv[2])) { fprintf(stderr, "XShm displays/markers unavailable\n"); return 2; }
  size_t size = (size_t)src.im->bytes_per_line * 720;
  char *slots[8]; int ids[8]; double ages[8] = {0};
  for (int i = 0; i < 8; i++) { slots[i] = malloc(size); ids[i] = -1; if (!slots[i]) return 2; }
  int last_src = -1, last_dst = -1, pairs = 0, seen = -1; double started = now(), stable_since = started;
  while (now() - started < atof(argv[3])) {
    int id = grab(&src, 0);
    if (id >= 0 && id != seen) { seen = id; stable_since = now(); }
    if (id >= 0 && id % stride == 0 && id != last_src && now() - stable_since >= settle && grab(&src, 1) == id) {
      int slot = (id / stride) % 8; memcpy(slots[slot], src.im->data, size); ids[slot] = id; ages[slot] = now() - stable_since; last_src = id;
    }
    id = grab(&dst, 0);
    if (id >= 0 && id % stride == 0 && id != last_dst && ids[(id / stride) % 8] == id && grab(&dst, 1) == id) {
      double scale = (double)(dst.right - dst.left) / (src.right - src.left);
      if (scale > .997 && scale < 1.003) scale = 1;
      if (scale != 1) { fprintf(stderr, "Quality capture requires 1:1 viewer scale, got %.6f\n", scale); return 3; }
      char file[4096]; char *original = src.im->data; src.im->data = slots[(id / stride) % 8]; src.im->height = 720;
      snprintf(file, sizeof(file), "%s-%d-source.ppm", argv[4], id);
      int ok = save(&src, file, 1120, 360, src.left + 1, src.row + 120);
      src.im->data = original;
      snprintf(file, sizeof(file), "%s-%d-client.ppm", argv[4], id);
      ok = ok && save(&dst, file, (int)(1120 * scale + .5), (int)(360 * scale + .5), dst.left + 1, dst.row + (int)(120 * scale + .5));
      if (!ok) { fprintf(stderr, "Invalid ROI or frame write failed\n"); return 3; }
      printf("P %d %.6f\n", id, scale); fflush(stdout); pairs++; last_dst = id;
      if (settle > 0) { printf("A %d %.6f\n", id, ages[(id / stride) % 8] * 1000); fflush(stdout); }
    }
    struct timespec delay = {0, 4000000}; nanosleep(&delay, NULL);
  }
  for (int i = 0; i < 8; i++) free(slots[i]);
  close_reader(&src); close_reader(&dst);
  if (!pairs) { fprintf(stderr, "No matched source IDs\n"); return 3; }
  return 0;
}
