class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.11.0"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.11.0/diffler-v0.11.0-aarch64-apple-darwin.tar.gz"
      sha256 "9eac100c7caa0dc173d7df8134f818af77fe783d70b43a6d00b02e37cd97974e"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.11.0/diffler-v0.11.0-x86_64-apple-darwin.tar.gz"
      sha256 "80cc2ff446ea02ad641cf99db6988bd92caf012f57efefd073b315d17d2d3e74"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.11.0/diffler-v0.11.0-aarch64-unknown-linux-musl.tar.gz"
      sha256 "2eb99ceadea9cc7e08f1dc2f52dd90ac85a347c303878f235b3983a347faf8cf"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.11.0/diffler-v0.11.0-x86_64-unknown-linux-musl.tar.gz"
      sha256 "80862b28215a8c05ebf229da0d47a82d49a493f2ea17e421d4a982a86cf0dd0e"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
