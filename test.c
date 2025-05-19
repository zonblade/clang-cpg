#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define MAX_LEN 64

void overflow_func(char *input) {
    char buf[32];
    // Stack buffer overflow: no bounds check on strcpy
    strcpy(buf, input);
    printf("Buffer content: %s\n", buf);
}

void format_string_func(char *user_format) {
    char greeting[128] = "Hello, ";
    // Uncontrolled format string: user can add format specifiers
    printf(user_format);  
    printf("\n");
}

char* integer_overflow_alloc(size_t count, size_t size) {
    // Possible integer overflow if count * size wraps
    size_t total = count * size;
    char *p = malloc(total);
    if (!p) exit(1);
    return p;
}

void improper_input_func(char *data) {
    // Improper input validation: assumes data is numeric
    int num = atoi(data);
    printf("Number is %d\n", num);
}

void uaf_and_double_free() {
    char *ptr = malloc(16);
    free(ptr);
    // Use-after-free: ptr is still used
    printf("Freed data: %s\n", ptr);
    // Double-free: freeing twice
    free(ptr);
}

int main(int argc, char *argv[]) {
    if (argc < 2) {
        printf("Usage: %s <input>\n", argv[0]);
        return 1;
    }
    overflow_func(argv[1]);
    format_string_func(argv[1]);
    char *buf = integer_overflow_alloc(1 << 31, sizeof(int));
    improper_input_func(argv[1]);
    uaf_and_double_free();
    free(buf);
    return 0;
}
