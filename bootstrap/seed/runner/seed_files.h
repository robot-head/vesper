/* The seed sees a virtual root containing only the files named on the CLI.
 * Guest paths are keys in this table; only the host-supplied pathname reaches
 * fopen. This keeps the seed portable to C11 hosts without directory handles.
 * Included by vwasm.c after the canonical ABI allocation helpers. */
#include <errno.h>
#include <limits.h>

typedef struct {
  const char *name, *host;
  int output;
} SeedFile;
static SeedFile *seed_files;
static size_t num_seed_files;

static int canonical_name(const char *name) {
  const char *p = name;
  for (;;) {
    size_t n = strcspn(p, "/");
    if (!n || (n == 1 && p[0] == '.') || (n == 2 && !memcmp(p, "..", 2))) return 0;
    if (!p[n]) return 1;
    p += n + 1;
  }
}

static void add_seed_file(const char *name, const char *path, int output) {
  if (*name == '/') name++;
  if (!canonical_name(name)) die(2, "invalid virtual file name %s", name);
  for (size_t i = 0; i < num_seed_files; i++)
    if (!strcmp(seed_files[i].name, name)) die(2, "duplicate virtual file %s", name);
  seed_files = xrealloc(seed_files, sizeof *seed_files * (num_seed_files + 1));
  seed_files[num_seed_files++] = (SeedFile){name, path, output};
}

enum { FS_ACCESS = 0, FS_BAD_FD = 3, FS_EXIST = 7, FS_INVALID = 12, FS_IO = 13,
       FS_NO_ENTRY = 20, FS_NO_MEMORY = 22, FS_NOT_DIR = 24, FS_UNSUPPORTED = 27,
       FS_OVERFLOW = 30 };
enum { ROOT_FD = 1, INPUT_FD, OUTPUT_FD, DIR_FD };
typedef struct { FILE *file; char *prefix; int kind; } SeedFd;
static SeedFd seed_fds[1024]; /* slot zero is always invalid */

typedef struct { char *prefix; size_t pos; int active; } SeedDirStream;
static SeedDirStream seed_streams[1024]; /* slot zero is always invalid */

static char *seed_strdup(const char *s, size_t n) {
  char *p = xmalloc(n + 1);
  if (n) memcpy(p, s, n);
  p[n] = 0;
  return p;
}

static uint32_t free_seed_stream(void) {
  for (uint32_t i = 1; i < sizeof seed_streams / sizeof seed_streams[0]; ++i)
    if (!seed_streams[i].active) return i;
  return 0;
}

static SeedDirStream *seed_stream(uint64_t id) {
  if (id >= sizeof seed_streams / sizeof seed_streams[0]) return &seed_streams[0];
  return &seed_streams[id];
}

static uint32_t free_seed_fd(void) {
  for (uint32_t i = 1; i < sizeof seed_fds / sizeof seed_fds[0]; ++i)
    if (!seed_fds[i].kind) return i;
  return 0;
}

static SeedFd *seed_fd(uint64_t fd) {
  if (fd >= sizeof seed_fds / sizeof seed_fds[0]) return &seed_fds[0];
  return &seed_fds[fd];
}

static void fs_error(uint8_t *result, unsigned payload, int code) {
  result[0] = 1;
  result[payload] = (uint8_t)code;
}

static void fs_u64(Vm *vm, uint64_t ptr, uint64_t n) {
  guest_put_u32(vm, ptr, (uint32_t)n);
  guest_put_u32(vm, ptr + 4, (uint32_t)(n >> 32));
}

static void host_directories(Vm *vm, uint64_t *a, uint64_t *r) {
  (void)r;
  uint32_t ret = (uint32_t)a[0], list = 0;
  if (num_seed_files) {
    list = guest_alloc(vm, 4, 12);
    guest_put_bytes(vm, (uint64_t)list + 4, "/", 1);
    uint32_t fd = free_seed_fd();
    if (!fd) TRAP("too many live filesystem descriptors");
    seed_fds[fd].kind = ROOT_FD;
    guest_put_u32(vm, list, fd);
  }
  guest_put_u32(vm, ret, list);
  guest_put_u32(vm, (uint64_t)ret + 4, num_seed_files != 0);
}

static int fs_errno(void) {
  if (errno == EACCES) return FS_ACCESS;
  if (errno == EEXIST) return FS_EXIST;
  if (errno == ENOENT) return FS_NO_ENTRY;
  return FS_IO;
}

static SeedFile *seed_lookup(const char *path, size_t len) {
  for (size_t i = 0; i < num_seed_files; ++i)
    if (strlen(seed_files[i].name) == len && !memcmp(seed_files[i].name, path, len)) return &seed_files[i];
  return NULL;
}

static int seed_dir_exists(const char *prefix, size_t len) {
  if (!len) return num_seed_files != 0;
  for (size_t i = 0; i < num_seed_files; ++i) {
    const char *nm = seed_files[i].name;
    if (strlen(nm) > len && !memcmp(nm, prefix, len) && nm[len] == '/') return 1;
  }
  return 0;
}

static int seed_resolve(const SeedFd *dir, const uint8_t *path, uint32_t len, char *out, size_t cap, size_t *out_len) {
  size_t plen = dir->prefix ? strlen(dir->prefix) : 0;
  while (len && path[0] == '/') { path++; len--; }
  while (len >= 2 && path[0] == '.' && path[1] == '/') { path += 2; len -= 2; }
  while (len && path[len - 1] == '/') len--;
  if (len == 1 && path[0] == '.') len = 0;
  size_t need = plen + (plen && len ? 1 : 0) + len;
  if (need >= cap) return 0;
  size_t pos = 0;
  if (plen) { memcpy(out, dir->prefix, plen); pos = plen; }
  if (plen && len) out[pos++] = '/';
  if (len) { memcpy(out + pos, path, len); pos += len; }
  out[pos] = 0;
  *out_len = pos;
  return 1;
}

static void host_open_at(Vm *vm, uint64_t *a, uint64_t *r) {
  (void)r;
  uint32_t n = (uint32_t)a[3], flags = (uint32_t)a[4], rights = (uint32_t)a[5];
  const uint8_t *path = mem_at(vm, (uint32_t)a[2], n);
  uint32_t ret = (uint32_t)a[6];
  uint8_t *result = mem_at(vm, ret, 8);
  SeedFd *parent = seed_fd(a[0]);
  if (parent->kind != ROOT_FD && parent->kind != DIR_FD) { fs_error(result, 4, FS_BAD_FD); return; }
  if (a[1] > 1) { fs_error(result, 4, FS_INVALID); return; }
  char resolved[4096];
  size_t rlen = 0;
  if (!seed_resolve(parent, path, n, resolved, sizeof resolved, &rlen)) { fs_error(result, 4, FS_INVALID); return; }
  if (flags & 2) {
    if (flags != 2) { fs_error(result, 4, FS_UNSUPPORTED); return; }
    if (seed_lookup(resolved, rlen)) { fs_error(result, 4, FS_NOT_DIR); return; }
    if (!seed_dir_exists(resolved, rlen)) { fs_error(result, 4, FS_NO_ENTRY); return; }
    uint32_t fd = free_seed_fd();
    if (!fd) { fs_error(result, 4, FS_NO_MEMORY); return; }
    seed_fds[fd] = (SeedFd){NULL, seed_strdup(resolved, rlen), DIR_FD};
    result[0] = 0;
    guest_put_u32(vm, (uint64_t)ret + 4, fd);
    return;
  }
  SeedFile *f = seed_lookup(resolved, rlen);
  if (!f) { fs_error(result, 4, FS_NO_ENTRY); return; }
  if (rights != (f->output ? 2u : 1u)) { fs_error(result, 4, FS_ACCESS); return; }

  /* open-flags: CREATE=1, DIRECTORY=2, EXCLUSIVE=4, TRUNCATE=8. */
  const char *mode = "rb";
  if (f->output && flags == 9) mode = "wb";
  else if (f->output && flags == 5) mode = "wbx";
  else if (f->output && (flags == 0 || flags == 1)) mode = "r+b";
  else if (f->output || flags != 0) { fs_error(result, 4, FS_UNSUPPORTED); return; }
  uint32_t fd = free_seed_fd();
  if (!fd) { fs_error(result, 4, FS_NO_MEMORY); return; }
  FILE *file = fopen(f->host, mode);
  if (!file && flags == 1 && errno == ENOENT) {
    file = fopen(f->host, "wbx");
    if (!file && errno == EEXIST) file = fopen(f->host, "r+b");
  }
  if (!file) { fs_error(result, 4, fs_errno()); return; }
  seed_fds[fd] = (SeedFd){file, NULL, f->output ? OUTPUT_FD : INPUT_FD};
  result[0] = 0;
  guest_put_u32(vm, (uint64_t)ret + 4, fd);
}

static void host_file_drop(Vm *vm, uint64_t *a, uint64_t *r) {
  (void)vm;
  (void)r;
  SeedFd *d = seed_fd(a[0]);
  if (!d->kind) TRAP("invalid filesystem descriptor");
  if (d->file && fclose(d->file)) TRAP("filesystem close failed");
  free(d->prefix);
  *d = (SeedFd){0};
}

static void file_stat_result(Vm *vm, uint32_t ret, FILE *file) {
  uint8_t *result = mem_at(vm, ret, 104);
  memset(result, 0, 104);
  long size = 0;
  if (file && (fseek(file, 0, SEEK_END) || (size = ftell(file)) < 0)) {
    fs_error(result, 8, FS_IO);
    return;
  }
  result[8] = file ? 6 : 3;  /* regular-file or directory */
  fs_u64(vm, (uint64_t)ret + 16, 1);
  fs_u64(vm, (uint64_t)ret + 24, (uint64_t)size);
}

static void host_file_stat(Vm *vm, uint64_t *a, uint64_t *r) {
  (void)r;
  SeedFd *d = seed_fd(a[0]);
  uint32_t ret = (uint32_t)a[1];
  if (!d->kind) { fs_error(mem_at(vm, ret, 104), 8, FS_BAD_FD); return; }
  file_stat_result(vm, ret, d->file);
}

static void host_file_stat_at(Vm *vm, uint64_t *a, uint64_t *r) {
  (void)r;
  uint32_t len = (uint32_t)a[3], ret = (uint32_t)a[4];
  uint8_t *result = mem_at(vm, ret, 104);
  const uint8_t *path = mem_at(vm, (uint32_t)a[2], len);
  SeedFd *parent = seed_fd(a[0]);
  if (parent->kind != ROOT_FD && parent->kind != DIR_FD) { fs_error(result, 8, FS_BAD_FD); return; }
  if (a[1] > 1) { fs_error(result, 8, FS_INVALID); return; }
  char resolved[4096];
  size_t rlen = 0;
  if (!seed_resolve(parent, path, len, resolved, sizeof resolved, &rlen)) { fs_error(result, 8, FS_INVALID); return; }
  if (!rlen || seed_dir_exists(resolved, rlen)) { file_stat_result(vm, ret, NULL); return; }
  SeedFile *f = seed_lookup(resolved, rlen);
  if (!f) { fs_error(result, 8, FS_NO_ENTRY); return; }
  FILE *file = fopen(f->host, "rb");
  if (!file) { fs_error(result, 8, fs_errno()); return; }
  file_stat_result(vm, ret, file);
  fclose(file);
}

static void host_read_directory(Vm *vm, uint64_t *a, uint64_t *r) {
  (void)r;
  SeedFd *d = seed_fd(a[0]);
  uint32_t ret = (uint32_t)a[1];
  uint8_t *result = mem_at(vm, ret, 8);
  if (d->kind != ROOT_FD && d->kind != DIR_FD) { fs_error(result, 4, FS_BAD_FD); return; }
  uint32_t id = free_seed_stream();
  if (!id) { fs_error(result, 4, FS_NO_MEMORY); return; }
  const char *p = d->prefix ? d->prefix : "";
  seed_streams[id] = (SeedDirStream){seed_strdup(p, strlen(p)), 0, 1};
  result[0] = 0;
  guest_put_u32(vm, (uint64_t)ret + 4, id);
}

static void host_read_directory_entry(Vm *vm, uint64_t *a, uint64_t *r) {
  (void)r;
  SeedDirStream *st = seed_stream(a[0]);
  uint32_t ret = (uint32_t)a[1];
  uint8_t *result = mem_at(vm, ret, 20);
  if (!st->active) { fs_error(result, 4, FS_BAD_FD); return; }
  size_t plen = strlen(st->prefix);
  while (st->pos < num_seed_files) {
    size_t i = st->pos++;
    const char *nm = seed_files[i].name;
    if (plen && (strlen(nm) <= plen || memcmp(nm, st->prefix, plen) || nm[plen] != '/')) continue;
    const char *rest = plen ? nm + plen + 1 : nm;
    size_t seglen = strcspn(rest, "/");
    if (!seglen) continue;
    int dup = 0;
    for (size_t j = 0; j < i; ++j) {
      const char *nmj = seed_files[j].name;
      if (plen && (strlen(nmj) <= plen || memcmp(nmj, st->prefix, plen) || nmj[plen] != '/')) continue;
      const char *restj = plen ? nmj + plen + 1 : nmj;
      if (strcspn(restj, "/") == seglen && !memcmp(restj, rest, seglen)) { dup = 1; break; }
    }
    if (dup) continue;
    uint32_t ptr = guest_alloc(vm, 1, (uint32_t)seglen);
    guest_put_bytes(vm, ptr, rest, (uint32_t)seglen);
    result = mem_at(vm, ret, 20);
    memset(result, 0, 20);
    result[4] = 1;
    result[8] = rest[seglen] == '/' ? 3 : 6;
    guest_put_u32(vm, (uint64_t)ret + 12, ptr);
    guest_put_u32(vm, (uint64_t)ret + 16, (uint32_t)seglen);
    return;
  }
  memset(result, 0, 20);
}

static void host_stream_drop(Vm *vm, uint64_t *a, uint64_t *r) {
  (void)vm;
  (void)r;
  SeedDirStream *st = seed_stream(a[0]);
  if (!st->active) TRAP("invalid directory entry stream");
  free(st->prefix);
  *st = (SeedDirStream){0};
}

/* Read at most one chunk per call. The emitted std/fs loops until full or
 * EOF. Keep host storage independent of guest memory across cabi_realloc. */
static void host_file_read(Vm *vm, uint64_t *a, uint64_t *r) {
  (void)r;
  SeedFd *d = seed_fd(a[0]);
  uint32_t ret = (uint32_t)a[3];
  uint8_t *result = mem_at(vm, ret, 16);
  if (d->kind != INPUT_FD) { fs_error(result, 4, FS_BAD_FD); return; }
  if (a[2] > LONG_MAX) { fs_error(result, 4, FS_OVERFLOW); return; }
  if (fseek(d->file, (long)a[2], SEEK_SET)) { fs_error(result, 4, FS_IO); return; }
  uint8_t chunk[16384];
  size_t want = a[1] < sizeof chunk ? (size_t)a[1] : sizeof chunk;
  size_t n = fread(chunk, 1, want, d->file);
  if (ferror(d->file)) { fs_error(result, 4, FS_IO); return; }
  int eof = feof(d->file);
  guest_put_bytes(vm, (uint64_t)ret + 4, chunk, (uint32_t)n);
  result = mem_at(vm, ret, 16);
  result[0] = 0;
  result[12] = eof != 0;
}

static void host_file_write(Vm *vm, uint64_t *a, uint64_t *r) {
  (void)r;
  SeedFd *d = seed_fd(a[0]);
  uint32_t len = (uint32_t)a[2], ret = (uint32_t)a[4];
  uint8_t *result = mem_at(vm, ret, 16);
  if (d->kind != OUTPUT_FD) { fs_error(result, 8, FS_BAD_FD); return; }
  if (a[3] > LONG_MAX) { fs_error(result, 8, FS_OVERFLOW); return; }
  const uint8_t *data = mem_at(vm, (uint32_t)a[1], len);
  if (fseek(d->file, (long)a[3], SEEK_SET) || fwrite(data, 1, len, d->file) != len || fflush(d->file)) {
    fs_error(result, 8, FS_IO);
    return;
  }
  result[0] = 0;
  fs_u64(vm, (uint64_t)ret + 8, len);
}
