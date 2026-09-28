class Diffler < Formula
  desc "Terminal code review for AI coding agents"
  homepage "https://github.com/matheusfillipe/diffler"
  version "0.16.0"
  license "MIT OR Apache-2.0"

  on_macos do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.16.0/diffler-v0.16.0-aarch64-apple-darwin.tar.gz"
      sha256 "be431e21871c79ac2d97fa86d1505038986b1a6270d195f771f41c9a93e1cd1d"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.16.0/diffler-v0.16.0-x86_64-apple-darwin.tar.gz"
      sha256 "473af6198e480e7d743b129145be55835aa277d28fa663bc1705fac5a62eb443"
    end
  end

  on_linux do
    on_arm do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.16.0/diffler-v0.16.0-aarch64-unknown-linux-musl.tar.gz"
      sha256 "78a27e8ce8edcbd0529ea8ff501a534598849bc84c74c802dd6a522432606e30"
    end
    on_intel do
      url "https://github.com/matheusfillipe/diffler/releases/download/v0.16.0/diffler-v0.16.0-x86_64-unknown-linux-musl.tar.gz"
      sha256 "48f58506c078e1437c1299c2399fff2ed7b210bbc948867fc032b9f063347752"
    end
  end

  def install
    bin.install Dir["**/diffler"].first => "diffler"
  end

  test do
    assert_match "diffler #{version}", shell_output("#{bin}/diffler --version")
  end
end
