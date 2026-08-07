class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.9.0"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.9.0/diffler-v0.9.0-aarch64-apple-darwin.tar.gz"
      sha256 "ff7a6f732d422b10924ba373cae28d346ab048fd2a3c2d6be7420e92061b6e4b"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.9.0/diffler-v0.9.0-x86_64-apple-darwin.tar.gz"
      sha256 "8f3881f1cd7ebee784778deb1c1b42ce9a481ee7c72701f748205c413b009dc8"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.9.0/diffler-v0.9.0-aarch64-unknown-linux-musl.tar.gz"
      sha256 "fec6132d67222e864de3523c7b12a4ae0315548560c772727086a5ab1a81a1d5"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.9.0/diffler-v0.9.0-x86_64-unknown-linux-musl.tar.gz"
      sha256 "42aac75d9e7ec7524ad513ecf30887427afce97e42042377fa2c07a32f003a7e"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
