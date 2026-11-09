#include "frame.h"
#include <CommonCrypto/CommonDigest.h>
#include <chrono>
#include <csignal>
#include <fcntl.h>
#include <fstream>
#include <iomanip>
#include <spawn.h>
#include <sstream>
#include <sys/wait.h>
#include <thread>
#include <unistd.h>
extern char **environ;
namespace frame {
ProcessResult run_process(const std::vector<std::string> &args,
                          const fs::path &log, int timeout) {
  std::vector<char *> argv;
  for (auto &a : args)
    argv.push_back(const_cast<char *>(a.c_str()));
  argv.push_back(nullptr);
  posix_spawn_file_actions_t actions;
  posix_spawn_file_actions_init(&actions);
  posix_spawn_file_actions_addopen(&actions, STDOUT_FILENO, log.c_str(),
                                   O_WRONLY | O_CREAT | O_TRUNC, 0600);
  posix_spawn_file_actions_adddup2(&actions, STDOUT_FILENO, STDERR_FILENO);
  posix_spawnattr_t attr;
  posix_spawnattr_init(&attr);
  posix_spawnattr_setflags(&attr, POSIX_SPAWN_SETPGROUP);
  posix_spawnattr_setpgroup(&attr, 0);
  pid_t pid;
  int err = posix_spawn(&pid, argv[0], &actions, &attr, argv.data(), environ);
  posix_spawnattr_destroy(&attr);
  posix_spawn_file_actions_destroy(&actions);
  if (err)
    throw Error(24, "cannot spawn Converter: " + std::to_string(err));
  auto start = std::chrono::steady_clock::now();
  bool expired = false;
  int status = 0;
  for (;;) {
    pid_t r = waitpid(pid, &status, WNOHANG);
    if (r == pid)
      break;
    if (r < 0 && errno != EINTR)
      throw Error(24, "waitpid failed");
    if (std::chrono::steady_clock::now() - start >
        std::chrono::seconds(timeout)) {
      expired = true;
      kill(-pid, SIGKILL);
      while (waitpid(pid, &status, 0) < 0 && errno == EINTR) {
      }
      break;
    }
    std::this_thread::sleep_for(std::chrono::milliseconds(20));
  }
  return {WIFEXITED(status) ? WEXITSTATUS(status) : 128 + WTERMSIG(status),
          expired};
}
std::string sha256(const fs::path &p) {
  std::ifstream in(p, std::ios::binary);
  if (!in)
    throw Error(20, "hash input open failed");
  CC_SHA256_CTX ctx;
  CC_SHA256_Init(&ctx);
  char b[65536];
  while (in) {
    in.read(b, sizeof b);
    if (auto n = in.gcount())
      CC_SHA256_Update(&ctx, b, CC_LONG(n));
  }
  if (!in.eof())
    throw Error(20, "hash read failed");
  unsigned char digest[CC_SHA256_DIGEST_LENGTH];
  CC_SHA256_Final(digest, &ctx);
  std::ostringstream out;
  out << std::hex << std::setfill('0');
  for (auto c : digest)
    out << std::setw(2) << unsigned(c);
  return out.str();
}
} // namespace frame
