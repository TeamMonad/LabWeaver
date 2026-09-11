#include <stdio.h>
#include <string.h>

#define MAX_RECORDS 64U
#define GRANTED "access-granted TRAINING_FLAG_OFFLINE_AUTH_DEMO"
#define DENIED "access-denied"

static int authenticate(const char *line)
{
    static const char expected_user[] = "operator";
    static const char expected_password[] = "training-password-42";
    const char *colon = strchr(line, ':');
    const char *candidate;
    size_t user_length;
    size_t password_length;

    if (colon == NULL || strchr(colon + 1, ':') != NULL) {
        return 0;
    }

    user_length = (size_t)(colon - line);
    if (user_length != sizeof(expected_user) - 1
        || memcmp(line, expected_user, user_length) != 0) {
        return 0;
    }

    candidate = colon + 1;
    password_length = strlen(candidate);
    if (password_length < 2 || candidate[password_length - 1] != '\n') {
        return 0;
    }
    password_length -= 1;
    if (password_length != sizeof(expected_password) - 1) {
        return 0;
    }
    return memcmp(expected_password, candidate, password_length) == 0;
}

int main(void)
{
    char line[128];
    unsigned int records = 0;

    while (records < MAX_RECORDS && fgets(line, sizeof(line), stdin) != NULL) {
        const size_t length = strlen(line);
        int complete = length > 0 && line[length - 1] == '\n';

        if (length == sizeof(line) - 1 && !complete) {
            int character;

            while ((character = fgetc(stdin)) != '\n' && character != EOF) {
                complete = 0;
            }
        }
        puts(complete && authenticate(line) ? GRANTED : DENIED);
        ++records;
    }
    return 0;
}
