class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.19.1"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.19.1/diffler-v0.19.1-aarch64-apple-darwin.tar.gz"
      sha256 "f64ac45df8368167478658423d8741d02b435d99523465be97736a4c442ec056"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.19.1/diffler-v0.19.1-x86_64-apple-darwin.tar.gz"
      sha256 "eacb162de817186d21edf30b5b42c5a054fb7a85431f46a1a07a0c29262d6f63"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.19.1/diffler-v0.19.1-aarch64-unknown-linux-musl.tar.gz"
      sha256 "f6e37f315a3ac46c7fc4cc9559d13b7d63fc28b8a09a3096cc3b0e47bfd51341"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.19.1/diffler-v0.19.1-x86_64-unknown-linux-musl.tar.gz"
      sha256 "f6144347176f432bd6e28b662d4d272023563d3d156eb88ca196e42fb1d63796"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
