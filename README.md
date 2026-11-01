# Ambar

Çift panelli arşiv yöneticisi: solda bilgisayarınız, sağda açık arşivin içi. Tauri 2, Rust ve Svelte 5 ile yazılmıştır. Arayüz Türkçedir, İngilizce de seçilebilir, tema sisteme uyar.

Şu an **aşama 1** tamamlandı: arşivleri okuma ve çıkartma. Ambar henüz arşiv oluşturmaz ve değiştirmez.

## Aşama 1'de neler var

- **Biçimler:** RAR (RAR4, RAR5, şifreli, başlıkları şifreli, parçalı, katı), ZIP (AES ve eski Türkçe adlar dahil), 7z (AES ve şifreli başlık dahil), TAR, GZ, BZ2, XZ. Biçim dosya adından değil, içeriğinden anlaşılır. BZ2 ve XZ dosyalarının boyutu önceden bilinemediği için listede tire görünür.
- **Gezinme:** sekmelerde birden çok arşiv, arşiv içinde klasörler, sıralama, klavye ile gezinme ve çoklu seçim. Gizli dosyalar Görünüm menüsünden açılır. Panel oranı ve bu seçim hatırlanır.
- **Büyük arşivler:** 100.000 girişli bir arşiv yarım saniyede açılır, liste akıcı kayar, pencere çıkartma sırasında da yanıt verir.
- **Çıkartma:** tümü veya seçilenler, klasör yapısıyla veya yapısız, var olan dosyalar için sor, üzerine yaz, atla ya da yeniden adlandır. Hasarlı dosyaları tutma seçeneği, ilerleme ve İptal. Çıkartılan dosyalar arşivdeki tarihini korur. Arşivden sol panele sürükleyerek de çıkartılır.
- **Toplu çıkartma:** sol panelde seçili dosyalarla Çıkart, hepsini sırayla çıkartır: parça 2, parça 10'dan önce gelir. Tümü aynı klasöre ya da her arşiv kendi adıyla ayrı klasöre gider. Şifreli arşiv parola sorar, kapatılan soru yalnızca o arşivi atlar. "Tümüne uygula" bütün toplu işe geçerlidir. İptal, o anki arşivi ve kalanları durdurur. Bir arşivin başarısız olması diğerlerini durdurmaz, sonuç penceresi toplamları ve sorunlu arşivleri gösterir. Parçalı bir RAR'ın bütün parçaları seçiliyse küme bir kez çıkartılır.
- **Komut satırı:** aynı program pencere açmadan da çalışır, aşağıdaki bölüme bakın.
- **Sınama ve Bilgi:** arşiv bütünlüğü ve biçim, kilit, kurtarma kaydı, açıklama gibi bilgiler.
- **Parola:** istenir, oturum boyunca yalnızca bellekte tutulur, diske, günlüğe ya da süreç argümanlarına yazılmaz.
- **Dosya açma:** arşivdeki dosya geçici klasöre çıkartılıp varsayılan uygulamasında açılır, Mac'te boşluk tuşu Quick Look ile önizler. Çalıştırılabilir olabilen türler önce onay ister. Geçici dosyalar kapanışta silinir.
- **Bırakma:** sistemden pencereye bırakılan arşivler açılır.
- **Güvenlik:** arşivdeki bir ad, seçilen hedefin dışına dosya yazamaz, bağlantı (symlink, hard link) girdileri oluşturulmaz ve raporda başarısız görünür. Başarısız bir işlem başarılı diye gösterilmez.

## Komut satırı

Örnek: bir klasördeki bütün Google Takeout parçalarını tek `data` klasörüne çıkartmak için

```sh
AMBAR=/Applications/Ambar.app/Contents/MacOS/ambar
$AMBAR extract ~/Downloads/takeout-*.zip --dest ~/Downloads/data
```

Kullanım:

```
ambar extract <arşiv>... --dest <klasör> [--overwrite skip|overwrite|rename] [--separate] [--no-paths] [--keep-broken] [--password-stdin]
ambar test <arşiv>... [--password-stdin]
ambar --help
ambar --version
```

- `--dest` zorunludur, klasör yoksa oluşturulur. Var olan dosyalar için varsayılan `skip`'tir ve son satırda yazar. `--separate` her arşivi hedefin altında kendi adıyla ayrı bir klasöre çıkartır.
- Arşivler verildiği sırayla değil, ada ve sayıya göre sırayla işlenir. Bir arşivin başarısız olması diğerlerini durdurmaz.
- **Parola argüman olarak verilmez**, çünkü argümanlar süreç listesinde görünür. `--password-stdin` standart girdiden bir satır okur ve parola isteyen her arşivde dener: `printf '%s\n' "$PAROLA" | $AMBAR extract a.zip --dest d --password-stdin`. Bu seçenek yoksa parola isteyen arşiv açık bir mesajla başarısız olur, diğerleri sürer.
- Çıktı İngilizcedir. Arşiv başına bir satır ok, skipped ve failed sayılarını verir, başarısızlar yol ve kodla listelenir, en son satır toplamdır. İlerleme yalnızca stderr bir terminalse, saniyede birkaç kez yazılır.
- Çıkış kodları: `0` hiçbir şey başarısız olmadı (yalnızca atlananlar da başarıdır), `1` bir öğe ya da arşiv başarısız oldu, `2` kullanım hatası, `130` iptal edildi.
- Ctrl+C o anki arşivi ve kalanları durdurur, yarım `.ambar-part` dosyaları silinir. İkinci kez basılırsa program beklemeden çıkar ve yarım dosya kalabilir.
- Komut satırı pencere ya da Dock simgesi açmaz. Yalnızca ilk argüman tam olarak `extract`, `test`, `--help`, `-h` ya da `--version` ise devreye girer, başka her durumda pencere açılır. **Windows'ta denenmedi:** sürüm derlemesi pencere uygulaması olduğundan üst konsola bağlanır, ama bu burada çalıştırılamadı.

## Henüz olmayanlar

- **RAR oluşturma** ve ZIP, 7z oluşturma. Araç çubuğundaki Ekle, Sil ve Onar düğmeleri bu yüzden kapalıdır.
- Duraklat ve kalan süre.
- Arşivde değişiklik (dosya ekleme, silme, yeniden adlandırma, açıklama, kilit, onarım).
- Arşivden masaüstüne sürükleme.
- Sistem bütünleşmesi: dosyaları Ambar ile çift tıkla açma, sağ tık menüleri.
- SFX, otomatik güncelleme.

## Sırada ne var

1. **Aşama 2, yazma:** `rar` aracının yönetimi, RAR, ZIP ve 7z oluşturma, ekleme, silme, yeniden adlandırma. Otomatik güncelleme ve CI bu aşamanın sonunda kurulur.
2. **Aşama 3, ileri özellikler:** kurtarma, onarım, parçalı arşiv oluşturma, SFX, kilit, yorum, profiller, arşivdeki dosyayı düzenleme. Mac'te arşivden sisteme sürükleme.
3. **Aşama 4, sistem:** dosya ilişkilendirme, sağ tık menüleri, tek uygulama örneği.
4. **Aşama 5, sürüm:** paketler ve dağıtım.

Ayrıntı: `docs/superpowers/specs/2026-11-01-ambar-design.md`.

## `rar` aracı gerekmez

Aşama 1'de Ambar'ı kullanmak için `rar` aracı **gerekmez**, uygulama onu içermez ve indirmez. RAR arşivlerini okumak ve çıkartmak uygulamanın içindeki UnRAR kütüphanesiyle yapılır. `rar` yalnızca geliştirici makinesinde, test arşivlerini yeniden üretmek için kullanılır.

## Mac'te ilk açılış

Ambar geçici (ad-hoc) imzalıdır, Apple Developer kimliğiyle imzalanmamış ve noter onayından geçmemiştir.

- Kendi makinenizde derlediğiniz uygulama doğrudan açılır.
- Başka yerden indirilen kopyayı macOS ilk açılışta engeller. Sistem Ayarları > Gizlilik ve Güvenlik bölümünde "Yine de Aç" düğmesine basın, ya da karantina işaretini kendiniz kaldırın: `xattr -dr com.apple.quarantine /Applications/Ambar.app`

## Platformlar

Bugüne dek yalnızca **Mac (Apple Silicon)** üzerinde çalıştırıldı. **Windows ve Linux'ta hiç çalıştırılmadı.** Kod bu sistemleri düşünerek yazıldı, ama derlenmeleri ve çalışmaları doğrulanmış değildir.

7z, Mac'te sürüm derlemesinin gerçek penceresinde denendi: düz ve başlıkları şifreli arşiv açıldı, düz arşiv sınandı, ikisi de çıkartıldı, içerik ve tarihler doğru çıktı. Bu arşivler `src-tauri/examples/make_7z.rs` ile üretildi. 7-Zip'in kendi yazdığı bir arşiv pencerede denenmedi, çünkü bu makinede 7-Zip yok.

## Derleme

Gerekenler: Rust, Node.js ve [Tauri 2 önkoşulları](https://tauri.app/start/prerequisites/). UnRAR kütüphanesi C++ olduğu için bir C++ derleyicisi de gerekir.

```sh
npm install
npm run tauri dev                       # geliştirme
npm run tauri build -- --bundles app    # Mac: src-tauri/target/release/bundle/macos/Ambar.app
```

Tauri'nin dmg adımı bu Mac'te başarısız olduğu için yalnızca uygulama paketi derlenir. İmzalama için ortam değişkeni gerekmez.

Denetimler:

```sh
cargo test --manifest-path src-tauri/Cargo.toml
npm test
npm run check
```

## Test arşivlerini yeniden üretme

`src-tauri/tests/fixtures/*.rar` depoda hazır gelir. Yeniden üretmek için (Mac arm64):

```sh
scripts/make-fixtures.sh
```

Betik, RARLAB'ın `rar` aracını git dışındaki `tests/.tools` klasörüne indirir. RAR4 arşivi için 6.x sürümünü de indirir, çünkü 7.x artık RAR4 yazmıyor. `rar` deneme sürümüdür, lisansı RARLAB'a aittir.

## Lisans notu

UnRAR source code is used to extract RAR archives, and may not be used to recreate the RAR compression algorithm.

© 2026 onuryrlmz
