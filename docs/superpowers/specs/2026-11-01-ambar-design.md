# Ambar: tasarım belgesi

Tarih: 2026-11-01. "Ambar", çalışma adıdır (Claude'un kararı).

## Amaç ve neden

Onur, Mac'teki RAR uygulamalarını "dandik" buluyor ve Windows'taki WinRAR'ın sunduğu her şeyi istiyor. Ambar, WinRAR'ın işlevlerini çift panelli bir arayüzle Mac, Windows ve Linux'a getiren masaüstü arşiv yöneticisidir. Öncelikle Onur'un kendi kullanımı içindir, Liman'dan bağımsızdır ve onunla kod paylaşmaz.

Başarı ölçütü: Onur, RAR ile ilgili bir iş için başka bir uygulamaya veya komut satırına ihtiyaç duymaz ve Mac'te uygulamayı "dandik" bulmaz.

## Kullanıcının kararları

- Mac, Windows ve Linux desteği ("Liman gibi").
- WinRAR'ın özelliklerinin tamamı hedeflenir.
- Ana pencere çift panellidir: solda bilgisayar, sağda arşiv (taslak C).
- RAR oluşturma, RARLAB'ın `rar` aracının deneme sürümüyle yapılır.

## Hedefler

1. Herhangi bir RAR arşivini (RAR4, RAR5, şifreli, parçalı) ek kurulum olmadan açmak, gezmek, sınamak ve çıkartmak.
2. `rar` aracı varken WinRAR ile tam uyumlu RAR arşivleri oluşturmak ve değiştirmek.
3. ZIP ve 7z oluşturmak ve açmak, derinliği "Format derinliği" tablosundaki gibi; TAR, GZ, BZ2 ve XZ açmak.
4. İşletim sistemiyle bütünleşmek: çift tık, sağ tık menüsü, sürükle-bırak.

## Hedef dışı

- WinRAR'ın görünüşünü, adını veya logosunu kopyalamak.
- Tema sistemi, antivirüs tarama düğmesi, reklam veya lisans ekranı taklidi.
- Bulut depolama ve hesap sistemi.
- ACE formatı (WinRAR da bıraktı).
- Yeni bir RAR sıkıştırıcısı yazmak. Bu, RARLAB lisansına aykırıdır.

## Kesinlikle olmaması gerekenler

- Kullanıcının dosyasını sessizce bozmak veya sormadan üzerine yazmak. Arşiv değiştirme işlemi yarıda kesilirse özgün arşiv sağlam kalmalıdır.
- Parolayı diske, günlüğe veya işlem argümanlarına yazmak.
- Arşivin içindeki `../` veya sembolik bağlantı hileleriyle hedef klasörün dışına dosya yazmak.
- Başarısız bir işlemi başarılı göstermek.
- `rar` aracını uygulamanın içine gömmek veya deneme süresini aşmaya çalışmak.

## Teknik temel (Claude'un kararı)

Tauri 2, Rust ve Svelte 5. Bu, Liman'ın yığınıdır; kullanıcı "Liman gibi" dedi ve bu yığının derleme, CI ve güncelleme süreçleri zaten biliniyor.

### Arşiv motorları

| Format | Motor | Not |
|---|---|---|
| RAR okuma, çıkartma, sınama | Gömülü UnRAR kodu (`unrar` veya `unrar-ng` crate'i, seçim geliştiricinin) | Ücretsiz, süre sınırı yok, `rar` gerektirmez |
| RAR oluşturma, değiştirme, onarım | RARLAB `rar` aracı, alt işlem olarak | Kullanıcının makinesine ayrı kurulur |
| ZIP, 7z, TAR, GZ, BZ2, XZ | Rust kütüphaneleri, seçim geliştiricinin | ZIP'te AES şifre desteği gerekir |

Motorlar tek bir Rust arayüzünün arkasında durur (listele, çıkart, sına, ekle, sil, yeniden adlandır). Arayüz, formatın desteklemediği işlemleri devre dışı gösterir.

Bu ayrım, kullanıcıya önceki konuşmada anlatılandan farklıdır: orada her şeyin `rar` aracıyla yapılacağı söylenmişti. Okumayı gömülü koda almak, 40 günlük deneme süresi bitse de arşiv açmanın serbest kalmasını sağlar. RAR için UnRAR, oluşturma için `rar` kullanılır.

### `rar` aracının yönetimi

2026-11-01 tarihinde doğrulanan bilgiler:

- RARLAB lisansı deneme süresini **en fazla 40 gün** olarak belirtir, sonrasında lisans satın alınması gerekir. Sürüm 7.23 çalıştırıldığında "Trial version" yazar ve işlemleri yapar. 40 gün sonra aracın teknik olarak ne yaptığı doğrulanmadı.
- Lisans, deneme sürümünün başka bir yazılım paketinin içinde dağıtılmasını yasaklar. Araç bu yüzden uygulamayla gelmez.
- Lisans metni, `rar` aracını çağıran bir arayüzden doğrudan söz etmez.
- Homebrew'daki `rar` paketi 2026-09-01'de devre dışı kalmıştır, çünkü Gatekeeper denetiminden geçmez.
- Mac arm64 paketi (7.23) ad-hoc imzalıdır ve `spctl` tarafından reddedilir. `curl` ile indirildiğinde karantina işareti almaz ve bu Mac'te (macOS 27.0.1) sorunsuz çalışmıştır. Tarayıcıyla indirilen kopya Gatekeeper'a takılabilir.
- `rar` yalnızca RAR yazar; ZIP desteklemez.
- Paketin içinde her platform için `default.sfx` vardır. Mac'te SFX oluşturma denendi ve çalıştı (`s.sfx` üretildi).
- Parola stdin borusundan verilemez ("Read error in the file stdin"). Mac'te `rar` parolayı terminalden ister.

Davranış:

1. RAR oluşturma isteği geldiğinde uygulama `rar` aracını arar: uygulamanın veri klasörü, PATH, Windows'ta `C:\Program Files\WinRAR\Rar.exe`.
2. Bulamazsa açıklama, lisans bağlantısı ve 40 günlük deneme bilgisini gösterip **İndir** düğmesi sunar.
3. Mac ve Linux'ta uygulama, RARLAB'ın resmî `tar.gz` paketini kendi veri klasörüne indirip açar. Sürüm ve SHA-256 uygulamada sabitlenir. Windows'ta RARLAB yalnızca yükleyici sunduğu için resmî WinRAR yükleyicisi indirilip kullanıcının onayıyla çalıştırılır.
4. Ayarlarda aracın yolu elle seçilebilir ve bulunan sürüm görünür. Lisans anahtarını kullanıcı isterse kendi yerleştirir.
5. Parola `rar` aracına PTY üzerinden verilir, hiçbir zaman `-p<parola>` argümanı olarak verilmez. Windows'taki yöntem geliştirme sırasında doğrulanmalıdır.

## Ana pencere

Üstte düğmeler: Ekle, Çıkart, Sına, Göster, Sil, Onar, Bilgi. Altında iki panel, en altta durum çubuğu.

- **Sol panel:** bilgisayardaki klasör. Sıralama, klavye ile gezinme, üst klasöre çıkma ve adres çubuğu.
- **Sağ panel:** açık arşivin içi. Ad, boyut, sıkışık boyut, oran, tarih, CRC ve şifre göstergesi. Arşiv içinde klasörlere girilir.
- Arşiv açık değilken sağ panel, arşiv açma veya oluşturma için bırakma alanı gösterir.
- Birden fazla arşiv sekmelerde açılabilir.
- Panel oranı değiştirilebilir ve hatırlanır.
- Soldan sağa sürükleme ekler, sağdan sola sürükleme çıkartır.
- Finder, Gezgin veya dosya yöneticisinden pencereye bırakılan dosyalar eklenir, bırakılan arşiv açılır.
- Tema sisteme uyar (açık ve koyu).
- Uzun işlemlerde ilerleme, kalan süre, duraklat ve iptal bulunur. İptal özgün arşivi bozmaz.
- Dil Türkçedir, metinler tek yerde tutulur ve İngilizce de eklenir.
- Hakkında penceresinde "by onuryrlmz" ve "© 2026 onuryrlmz" yazar.

## Özellik listesi

**Oluşturma:** format (RAR5, RAR4, ZIP, 7z), sıkıştırma seviyesi, sözlük boyutu, solid arşiv, parçalı arşiv (hazır ve özel boyutlar), kurtarma kaydı, kurtarma hacimleri, şifreleme, dosya adlarını şifreleme, BLAKE2 sağlama, arşiv yorumu, dışlama maskeleri, sonra özgün dosyaları silme, her dosya/klasör için ayrı arşiv, yeniden kullanılabilir ayar profilleri.

**Çıkartma:** seçili dosyalar veya tümü, hedef klasör, üzerine yazma sorusu (sor, yaz, atla, yeniden adlandır), yollarla veya yolsuz, parola isteme ve oturum boyunca bellekte tutma, parçalı arşivlerde eksik parçayı bildirme, hasarlı dosyaları saklama seçeneği.

**Arşiv yönetimi:** sınama, bilgi, yorum düzenleme, dosya silme, yeniden adlandırma, dosya ekleme, kilitleme, parola ekleme veya değiştirme, onarma, kurtarma hacimlerinden yeniden oluşturma, SFX'e dönüştürme, format dönüştürme, arşivde dosya arama.

**Arşivdeki dosyayı açma:** dosya geçici klasöre çıkartılıp varsayılan uygulamasında açılır. Dosya değişirse uygulama arşivi güncellemeyi sorar. Geçici dosyalar kapanışta silinir.

**Sistem bütünleşmesi:** tüm desteklenen uzantılar için çift tıkla açma. Sağ tık menüsünde "Buraya çıkart", "Klasöre çıkart" ve "Arşive ekle". Mac'te Finder hizmeti, Windows'ta Gezgin kayıtları. Linux'ta `.desktop` işlemleri, ayrıca Nautilus ve Dolphin için olabildiğince destek.

**SFX:** yalnızca `rar` aracının o sistemde sunduğu modül kadar. Mac'te Windows `.exe` SFX üretilmez ve arayüz bunu açıkça söyler.

## Hata davranışı

- Yanlış parola, eksik parça, hasarlı arşiv, dolu disk, izin hatası ve `rar` aracının bulunmaması ayrı ve anlaşılır Türkçe mesajlar verir.
- `rar` çıkış kodları belgedeki anlamlarıyla eşlenir, ham çıktı "Ayrıntılar" altında görünür (parola olmadan).
- Arşiv değiştiren işlemler önce aynı klasörde geçici bir kopya üzerinde çalışır, başarılı olunca değiştirir. Kopya için yer yoksa kullanıcıya söylenir.

## Sınama

- Motor testleri: gerçek `rar` ile üretilmiş sabit örnek arşivler (RAR4, RAR5, şifreli, başlığı şifreli, solid, parçalı, hasarlı, Türkçe ve emoji içeren adlar, 4 GB üzeri için ayrı ve yavaş bir test).
- Güvenlik testleri: yol geçişi ve sembolik bağlantı içeren kötü niyetli arşivler.
- Parola sızıntısı testi: işlem argümanlarında ve günlüklerde parola bulunmamalıdır.
- Kesinti testi: işlem öldürüldüğünde özgün arşiv aynı kalmalıdır.
- Ters uyumluluk: oluşturulan RAR, ZIP ve 7z dosyaları `rar`, `unzip` ve `7zz` ile sınanır.
- Arayüz, gerçek pencerede ve üç sistemde elle denenir. Denenemeyen sistemler sürüm notunda açıkça belirtilir.

## Dağıtım (Claude'un kararı)

- Proje yeri: `~/Desktop/OnurGithub/Ambar`, depo: `github.com/onuryrlmz/Ambar` (depo henüz oluşturulmadı ve oluşturmadan önce kullanıcıya sorulacak; otomatik güncelleme ve ücretsiz CI için herkese açık olması önerilir).
- Liman süreci izlenir: etiketle CI paketi, Mac arm64 ve Intel, Windows msi/exe, Linux deb/rpm/AppImage.
- `bundle.macOS.signingIdentity: "-"` kullanılır, ücretli kod imzası yoktur, ilk açılışta işletim sistemi uyarı verebilir.
- Ayrı bir güncelleme anahtarı üretilir (`~/.tauri/ambar.key`). Her sürümden sonra `latest.json` imzaları indirilen dosyalara karşı doğrulanır.

## Uygulama sırası (Claude'un kararı)

1. **Okuma:** çift panel, RAR/ZIP/7z/TAR açma, gezinme, çıkartma, sınama, parola.
2. **Yazma:** `rar` yönetimi, RAR ve ZIP/7z oluşturma, ekleme, silme, yeniden adlandırma, ilerleme ve iptal.
3. **İleri özellikler:** kurtarma, onarım, parçalı arşiv, SFX, kilit, yorum, profiller, arşivdeki dosyayı düzenleme.
4. **Sistem:** dosya ilişkilendirme, sağ tık menüleri, tek uygulama örneği.
5. **Sürüm:** CI, paketler, otomatik güncelleme.

## Bilinen açık konular

- **Arşivden masaüstüne sürükleme:** Liman'da `tauri-plugin-drag` asenkron çıkartma sonrası sürükleme başlatamadı ve yerel kod gerekti (Mac'te `NSFilePromiseProvider`, Windows'ta sanal dosyalar). Panel içi sürükleme ilk sürümde vardır; sisteme sürükleme son aşamaya bırakılır, o zamana kadar "Çıkart" kullanılır.
- **Daha az yaygın formatlar:** ISO, CAB, ARJ, LZH, UUE ve Z, temel özellikler bittikten sonra eklenir. WinRAR "tamamı" hedefinin son parçasıdır.
- **Windows ve Linux sınaması:** bu Mac'te yalnızca CI ile sınanabilir, gerçek makinede kullanım ayrıca denenmelidir.

## Değerlendirme sonrası kararlar (Claude'un kararı)

**"Dandik" hissi.** Kullanıcı somut bir kusur saymadı, bu yüzden üç riske de önlem alınır:

- Büyük arşivler ilk aşamadan itibaren akıcı olmalıdır: liste sanallaştırılır, 100.000 girişli bir arşivde gezinme takılmaz, listeleme arka planda yürür.
- Pencere yerel davranır: yerel menü çubuğu, Mac kısayolları, Quick Look (Mac'te boşluk tuşu) ve sisteme uyan tema.
- Özellik listesi, aşağıdaki kapsam kuralıyla tamamlanır.

**Kapsam.** Yukarıdaki özellik listesi bağlayıcıdır. Ek olarak SFX seçenekleri, zaman ve öznitelik seçenekleri ve iş kuyruğu eklenir. Sihirbaz ve hız testi (benchmark) son aşamadadır.

**Sürüm sırası.** Otomatik güncelleme ve CI, aşama 5'te değil, aşama 2 bitince kurulur, çünkü Onur uygulamayı o noktadan itibaren kullanabilir. Aşamalar ayrı sürümler olarak çıkar. Sistem bütünleşmesi aşama 4'te kalır. Ana pencereye sisteme sürükleme, Mac için aşama 3'e alınır.

**Platform engeli.** Mac sorunları sürümü durdurur. Windows ve Linux CI testleri geçmelidir; gerçek makinede denenmemiş kısımlar sürüm notuna yazılır ve sürümü durdurmaz.

**Format derinliği.** Öncelik RAR'dadır.

| İşlem | RAR | ZIP | 7z |
|---|---|---|---|
| Oluşturma, açma, sınama | var | var | var |
| Şifreleme | var | AES | AES |
| Ekleme, silme, yeniden adlandırma | var | var | yeniden yazarak, kütüphane elverirse; yoksa devre dışı |
| Parçalı, kurtarma, kilit, SFX, onarım | var | yok | yok |

Kütüphanenin desteklemediği işlemler arayüzde devre dışı görünür, `7zz` gibi harici araç eklenmez.

**Windows'ta parola.** PTY yöntemi aşama 2'nin başında Windows CI üzerinde denenir. Çalışmazsa argüman yasağı esnetilmez: Windows'ta RAR şifreleme, açık bir açıklamayla devre dışı kalır. Çıkartmadaki parolalar UnRAR kütüphanesiyle işlendiği için bundan etkilenmez.

**Testlerde `rar`.** Örnek arşivler önceden üretilip depoya konur. CI'da gerçek `rar` yalnızca ters uyumluluk için, depoya gömülmeden çalıştırılır.

**Taslak:** `2026-11-01-ana-pencere-taslaklari.html` dosyasındaki C seçeneği.

## Geliştiriciye bırakılanlar

Crate seçimleri, Rust arayüz ayrıntıları, Svelte bileşen yapısı, simge seti, klavye kısayolları (WinRAR ve Mac alışkanlıkları gözetilerek), ayar dosyası biçimi.
