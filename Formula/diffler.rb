class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.2.7"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.7/diffler-v0.2.7-aarch64-apple-darwin.tar.gz"
      sha256 "4fe4d9072e9bd45e19892e22c66a5ca5ad60d513e91779fe047f97793cfa23e8"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.7/diffler-v0.2.7-x86_64-apple-darwin.tar.gz"
      sha256 "73cc74ed8777cc710357d8af0c8e6799c191cbca038757f5cbd0a2fceff05605"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.7/diffler-v0.2.7-aarch64-unknown-linux-musl.tar.gz"
      sha256 "10ef2d484ee28007a4ebde17eb01096454c8fbca6aadd79c139dd9b9d06da857"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.2.7/diffler-v0.2.7-x86_64-unknown-linux-musl.tar.gz"
      sha256 "11645abda6d6e5157bb5b55716a20d4b58c27694b51712b2cd2695d88aea6174"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
