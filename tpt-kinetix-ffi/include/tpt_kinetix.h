#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>

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
