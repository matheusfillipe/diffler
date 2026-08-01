class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.6.7"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.7/diffler-v0.6.7-aarch64-apple-darwin.tar.gz"
      sha256 "9fb68192564a3d05bbc63a2bcc7b98ea13858e4197e233e34938c63a332b6fa1"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.7/diffler-v0.6.7-x86_64-apple-darwin.tar.gz"
      sha256 "b8227c22782ac6930d78ea15fc405e6fe5c5a1ec981f549496187e9a9a42e0a9"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.7/diffler-v0.6.7-aarch64-unknown-linux-musl.tar.gz"
      sha256 "58ee18b63e7d6e8d86ddf9da1e2d27af9d2f6437681b5d87dde740f8463627ea"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.6.7/diffler-v0.6.7-x86_64-unknown-linux-musl.tar.gz"
      sha256 "de99dbb91827519e4ea5e9599c14d6e9ced1dfeebc9b0fb249fda9e3db0b2687"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
