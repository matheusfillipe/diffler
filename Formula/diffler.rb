class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.1.8"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.8/diffler-v0.1.8-aarch64-apple-darwin.tar.gz"
      sha256 "acc4277205ba23f97f2c595874c72277b87b5ff1cbc33a29cf71d5ba5e60ecf9"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.8/diffler-v0.1.8-x86_64-apple-darwin.tar.gz"
      sha256 "dc46a52eb600f04bc0cb5c95d53b1ee8d9e412d8df019de1ae0548b130078357"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.8/diffler-v0.1.8-aarch64-unknown-linux-musl.tar.gz"
      sha256 "1ef980a888374c88431673c4921a2941bb77fe89799d08eb0d24e41c4bf26775"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.1.8/diffler-v0.1.8-x86_64-unknown-linux-musl.tar.gz"
      sha256 "112488dbb2d95cfe04064d9882f893916f3aa9ab2c7f802ded472317cf3d964c"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
