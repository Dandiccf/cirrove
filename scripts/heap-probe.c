/* Sample glibc's allocator state at each capacity-fixture marker.
 *
 * The capacity fixtures report process RSS and PSS, which cannot distinguish
 * live application data from memory the application has freed and glibc has not
 * returned to the kernel. Both appear as resident bytes, and the namespace
 * memory gate turns on telling them apart: a live-data problem is bounded by
 * capping resident views, an allocator-retention problem is not, and neither
 * remedy addresses the other.
 *
 * This interposes write(2), and whenever a fixture prints one of its markers to
 * stdout it appends a mallinfo2 sample to CIRROVE_HEAP_PROBE_LOG. Sampling on
 * the marker rather than on a timer pairs each allocator sample with exactly one
 * fixture phase, so the two series can be joined by line number.
 *
 * Diagnostic only. Never loaded by the service or by CI.
 *
 * Build:
 *   gcc -O2 -fPIC -shared -o heap-probe.so scripts/heap-probe.c -ldl
 * Use, from a frozen release test binary:
 *   TMPDIR=<disk-backed> SQLITE_TMPDIR=<disk-backed> \
 *   LD_PRELOAD=$PWD/heap-probe.so CIRROVE_HEAP_PROBE_LOG=$PWD/heap.jsonl \
 *   CIRROVE_CHURN_FILES=500000 <binary> --exact \
 *     filesystem::capacity::real_combined_namespace_churn --nocapture --ignored
 *
 * TMPDIR must not be tmpfs. A RAM-backed temporary directory moves bytes out of
 * process RSS without reducing host memory, which would flatter any result.
 */
#define _GNU_SOURCE
#include <fcntl.h>
#include <malloc.h>
#include <stdlib.h>
#include <string.h>
#include <sys/syscall.h>
#include <time.h>
#include <unistd.h>

static int output = -1;

__attribute__((constructor)) static void init_probe(void) {
    const char *path = getenv("CIRROVE_HEAP_PROBE_LOG");
    if (path) output = open(path, O_WRONLY | O_CREAT | O_TRUNC | O_CLOEXEC, 0600);
}

ssize_t write(int fd, const void *buffer, size_t size) {
    /* The real write happens first and its result is returned unchanged, so a
     * failure to sample can never alter what the fixture observes. */
    ssize_t result = syscall(SYS_write, fd, buffer, size);
    if (output >= 0 && fd == STDOUT_FILENO &&
        (memmem(buffer, size, "CIRROVE_NAMESPACE_CAPACITY", 26) ||
         memmem(buffer, size, "CIRROVE_PROJECTION_PAYLOAD", 26) ||
         memmem(buffer, size, "CIRROVE_COMBINED_CHURN", 22))) {
        struct mallinfo2 memory = mallinfo2();
        struct timespec now;
        clock_gettime(CLOCK_MONOTONIC, &now);
        char record[512];
        int n = snprintf(record, sizeof record,
            "{\"monotonic_sec\":%ld.%09ld,\"arena_bytes\":%zu,"
            "\"allocated_arena_bytes\":%zu,\"free_arena_bytes\":%zu,"
            "\"mmap_bytes\":%zu}\n",
            now.tv_sec, now.tv_nsec, memory.arena, memory.uordblks,
            memory.fordblks, memory.hblkhd);
        if (n > 0 && (size_t)n < sizeof record)
            syscall(SYS_write, output, record, n);
    }
    return result;
}
