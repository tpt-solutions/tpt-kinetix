/* C smoke test for the tpt-kinetix C ABI.
 *
 * Usage: smoke <media file> <cenc media file> <scratch directory>
 * Probes the file, writes a faststart copy, a progressive remux and an HLS + DASH
 * package into the scratch directory, then packages the CENC media (CENC only
 * covers VP9, AV1 and Opus samples, so it needs its own input) with a key. Checks
 * that a missing file fails with an error message. Exit status 0 on success.
 */
#include <stdio.h>
#include <string.h>

#include "tpt_kinetix.h"

#define KEY_ID "00112233445566778899aabbccddeeff"
#define KEY "ffeeddccbbaa99887766554433221100"

static int path_in(char *buf, size_t len, const char *dir, const char *name) {
    int n = snprintf(buf, len, "%s/%s", dir, name);
    return n > 0 && (size_t)n < len;
}

static int fail(const char *what) {
    fprintf(stderr, "%s failed: %s\n", what, tk_last_error());
    return 1;
}

int main(int argc, char **argv) {
    if (argc != 4) {
        fprintf(stderr, "usage: %s <media file> <cenc media file> <scratch directory>\n", argv[0]);
        return 2;
    }
    const char *media = argv[1];
    const char *cenc_media = argv[2];
    const char *dir = argv[3];
    char out[4096], out2[4096], pkg[4096], enc[4096];
    struct TkPackageSummary summary;

    printf("tpt-kinetix %s\n", tk_version());

    char *json = tk_probe_json(media);
    if (json == NULL) return fail("probe");
    if (strstr(json, "\"streams\":[") == NULL) {
        fprintf(stderr, "unexpected probe output: %s\n", json);
        tk_free_string(json);
        return 1;
    }
    printf("%s\n", json);
    tk_free_string(json);

    if (!path_in(out, sizeof out, dir, "faststart.mp4") ||
        !path_in(out2, sizeof out2, dir, "remux.mp4") ||
        !path_in(pkg, sizeof pkg, dir, "package") ||
        !path_in(enc, sizeof enc, dir, "package-cenc")) {
        fprintf(stderr, "scratch path too long\n");
        return 1;
    }

    if (tk_faststart(media, out) != 0) return fail("faststart");
    if (tk_remux(media, out2, 0, 0) != 0) return fail("remux");

    if (tk_package(media, pkg, 2.0, NULL, NULL, NULL, &summary) != 0) return fail("package");
    printf("package: %u track(s), %u segment(s), %llu file(s), %llu byte(s)\n",
           summary.tracks, summary.segments,
           (unsigned long long)summary.files, (unsigned long long)summary.bytes);
    if (summary.tracks == 0 || summary.files == 0) {
        fprintf(stderr, "package wrote nothing\n");
        return 1;
    }

    if (tk_package(cenc_media, enc, 2.0, KEY_ID, KEY, "cenc", &summary) != 0) return fail("cenc package");
    if (tk_package(cenc_media, enc, 2.0, KEY_ID, NULL, NULL, NULL) == 0) {
        fprintf(stderr, "a half-specified key unexpectedly packaged\n");
        return 1;
    }

    if (tk_probe_json("this/file/does/not/exist.mp4") != NULL) {
        fprintf(stderr, "a missing file unexpectedly probed\n");
        return 1;
    }
    if (tk_last_error() == NULL) {
        fprintf(stderr, "a failed call left no error message\n");
        return 1;
    }
    printf("error path: %s\n", tk_last_error());
    return 0;
}
