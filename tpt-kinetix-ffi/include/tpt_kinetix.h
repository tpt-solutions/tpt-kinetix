#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

/**
 * Counts reported by [`tk_package`].
 */
typedef struct TkPackageSummary {
  /**
   * Number of packaged tracks.
   */
  uint32_t tracks;
  /**
   * Number of segments per track.
   */
  uint32_t segments;
  /**
   * Files written into the output directory.
   */
  uint64_t files;
  /**
   * Bytes of media written.
   */
  uint64_t bytes;
} TkPackageSummary;

/**
 * The library version, e.g. `0.1.2`. The string is static: do not free it.
 */
const char *tk_version(void);

/**
 * The message of the last failed call on this thread, or null when there is none.
 * The pointer stays valid until the next call on this thread that fails.
 */
const char *tk_last_error(void);

/**
 * Frees a string returned by this library. Null is accepted and ignored.
 *
 * # Safety
 * `s` must be null or a pointer returned by [`tk_probe_json`] and not freed yet.
 */
void tk_free_string(char *s);

/**
 * Lists the tracks of the file at `path` as JSON, or returns null on error.
 *
 * The document has a `format` string and a `streams` array; each stream has
 * `index`, `codec`, `type` (`video`, `audio` or `other`), and for video `width` and
 * `height`, for audio `channels` and `sample_rate`. Free the result with
 * [`tk_free_string`].
 *
 * # Safety
 * `path` must be a NUL-terminated UTF-8 string.
 */
char *tk_probe_json(const char *path);

/**
 * Writes a copy of `input` to `output` with the index moved to the front.
 * Returns 0 on success and -1 on error.
 *
 * # Safety
 * `input` and `output` must be NUL-terminated UTF-8 strings.
 */
int tk_faststart(const char *input, const char *output);

/**
 * Copies the streams of `input` into a new file at `output` without decoding.
 *
 * The output container follows the extension of `output`: `.webm` and `.mkv` write
 * WebM, anything else writes MP4. `faststart` moves the index to the front of an
 * MP4. `fragment_ms` greater than zero writes fragmented MP4 with that target
 * fragment length; pass zero for a progressive file. Returns 0 on success and -1 on
 * error.
 *
 * # Safety
 * `input` and `output` must be NUL-terminated UTF-8 strings.
 */
int tk_remux(const char *input, const char *output, int faststart, uint32_t fragment_ms);

/**
 * Writes an HLS + DASH package of `input` into the directory `out_dir` (created if
 * missing). `segment_seconds` must be positive; segments cut on key frames.
 *
 * Encryption is off when both `key_id` and `key` are null. Otherwise both are
 * 32-digit hex strings (16 bytes each) and `scheme` is `"cenc"` or `"cbcs"` (null
 * means `"cenc"`). `summary`, when not null, receives the counts. Returns 0 on
 * success and -1 on error.
 *
 * # Safety
 * String arguments must be null or NUL-terminated UTF-8. `summary` must be null or
 * point to writable memory for a [`TkPackageSummary`].
 */
int tk_package(const char *input,
               const char *out_dir,
               double segment_seconds,
               const char *key_id,
               const char *key,
               const char *scheme,
               struct TkPackageSummary *summary);
