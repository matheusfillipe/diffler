class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.18.0"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.18.0/diffler-v0.18.0-aarch64-apple-darwin.tar.gz"
      sha256 "d48d4f9e5bb1ccdaeb841b769554abe3db1662e4d1b8cc40082c7521c0a4758c"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.18.0/diffler-v0.18.0-x86_64-apple-darwin.tar.gz"
      sha256 "5c122e1d14d4fdc00ce133ecc2ae4efdd27dfb274da25da82dc787e2414ef8c2"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.18.0/diffler-v0.18.0-aarch64-unknown-linux-musl.tar.gz"
      sha256 "c409a246349e5344c002bcdc7484a5f2f68a8cf2093d87a56dffe4daaef27bfd"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.18.0/diffler-v0.18.0-x86_64-unknown-linux-musl.tar.gz"
      sha256 "702d0cd19766ae0ca03182126524c3cac00b9276fda221011661d2759cc945bb"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
