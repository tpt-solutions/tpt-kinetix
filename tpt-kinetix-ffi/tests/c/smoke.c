/* C smoke test for the tpt-kinetix C ABI.
 *
 * Usage: smoke <media file> <scratch output path>
 * Probes the file, writes a faststart copy to the scratch path, and checks that
 * a missing file fails with an error message. Exit status 0 on success.
 */
#include <stdio.h>
#include <string.h>

#include "tpt_kinetix.h"

int main(int argc, char **argv) {
    if (argc != 3) {
        fprintf(stderr, "usage: %s <media file> <output file>\n", argv[0]);
        return 2;
    }
    printf("tpt-kinetix %s\n", tk_version());

    char *json = tk_probe_json(argv[1]);
    if (json == NULL) {
        fprintf(stderr, "probe failed: %s\n", tk_last_error());
        return 1;
    }
    if (strstr(json, "\"streams\":[") == NULL) {
        fprintf(stderr, "unexpected probe output: %s\n", json);
        tk_free_string(json);
        return 1;
    }
    printf("%s\n", json);
    tk_free_string(json);

    if (tk_faststart(argv[1], argv[2]) != 0) {
        fprintf(stderr, "faststart failed: %s\n", tk_last_error());
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
