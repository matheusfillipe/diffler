class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.4.0"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.4.0/diffler-v0.4.0-aarch64-apple-darwin.tar.gz"
      sha256 "3bd7de513db0f099edd41dc18f5c9c19f76cede53c84005fdaed944d13710149"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.4.0/diffler-v0.4.0-x86_64-apple-darwin.tar.gz"
      sha256 "c8ba337c0203a9e68fe99eac3b408acf2ce50d358aaf0787dc8591202d199dfe"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.4.0/diffler-v0.4.0-aarch64-unknown-linux-musl.tar.gz"
      sha256 "58d1b54763279449047e60d874cdad934206149c72381c1dece1f542d751e87e"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.4.0/diffler-v0.4.0-x86_64-unknown-linux-musl.tar.gz"
      sha256 "163b184a3289c800f6950f63c99298184fd8af4fbc6b4f37cc972bd7ce88b1ee"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
