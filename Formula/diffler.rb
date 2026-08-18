class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.12.0"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.12.0/diffler-v0.12.0-aarch64-apple-darwin.tar.gz"
      sha256 "0eb12facb43f1bf1ca7c728836342b81642b725bdb467db1ba6d2abac643e0e7"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.12.0/diffler-v0.12.0-x86_64-apple-darwin.tar.gz"
      sha256 "455de87baf75e33ce8f0a9779061b64ef49c6da5313a8eee32aa069e079d0afc"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.12.0/diffler-v0.12.0-aarch64-unknown-linux-musl.tar.gz"
      sha256 "676b932f78c1951e646170906d91f3fba3c5cbdbcf663d60e25697c11c686444"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.12.0/diffler-v0.12.0-x86_64-unknown-linux-musl.tar.gz"
      sha256 "7caf61cd4d48a6fdb5bcf3b716a5955951000f9774070eeb1e7a740281c960c9"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
