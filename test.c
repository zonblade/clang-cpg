#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define BASE_PATH "./files/"
#define MAX_FILENAME 256

// Rejects filename if it contains "..", starts with '/', or is absolute path
int isValidFilename(const char *filename) {
    if (strstr(filename, "..") != NULL) {
        return 0;
    }
    if (filename[0] == '/') {
        return 0;
    }
#ifdef _WIN32
    if (strstr(filename, ":") != NULL) { // prevent C:\ paths on Windows
        return 0;
    }
#endif
    return 1;
}

void readFile(const char *filename) {
    char fullPath[512];

    if (!isValidFilename(filename)) {
        printf("Invalid filename: path traversal detected.\n");
        return;
    }

    snprintf(fullPath, sizeof(fullPath), "%s%s", BASE_PATH, filename);

    FILE *fp = fopen(fullPath, "r");
    if (fp == NULL) {
        perror("Error opening file");
        return;
    }

    char ch;
    while ((ch = fgetc(fp)) != EOF) {
        putchar(ch);
    }
    fclose(fp);
}

int main(int argc, char *argv[]) {
    if (argc != 2) {
        printf("Usage: %s <filename>\n", argv[0]);
        return 1;
    }

    readFile(argv[1]);
    return 0;
}
