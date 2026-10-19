# MDISec — Güvenlik testi daveti

**Konu:** nlink ödeme ve abonelik altyapısının güvenlik (sızma) testi
**Hazırlayan:** Ramazan ERTAN (nlink)
**Durum:** Davet — proforma fatura ve KOSGEB onayı bekleniyor

Merhaba MDISec ekibi,

nlink'in ödeme ve abonelik altyapısını bağımsız bir güvenlik testinden geçirmek istiyoruz. Testin KOSGEB desteğiyle yapılmasını planlıyoruz.

Proforma fatura onaylanıp KOSGEB desteği kesinleştiğinde, sizi aşağıdaki kapsamda projemizi test etmeye davet ediyoruz.

## Süreç

1. **Proforma fatura:** Kapsam ve süreye göre teklifinizi ve proforma faturanızı bekliyoruz.
2. **KOSGEB onayı:** Proforma fatura ile KOSGEB başvurusu yapılacak. Onay gelince size yazılı olarak bildirilecek.
3. **Başlangıç toplantısı:** Kapsam, test penceresi, iletişim kişileri ve kurallar birlikte netleştirilecek.
4. **Erişimlerin verilmesi:** Test ortamı adresleri, test hesapları ve gerekli belgeler bu aşamada güvenli bir kanaldan paylaşılacak.
5. **Test:** Kararlaştırılan pencerede yapılacak.
6. **Rapor ve doğrulama:** Bulgular raporlanacak. Düzeltmelerden sonra yeniden test (retest) yapılacak.

> Testler, KOSGEB onayı ve başlangıç toplantısı tamamlanmadan başlamaz. Erişim bilgileri yalnız bu adımlardan sonra paylaşılır.

## Kapsam

Bu depo **paytr-service** projesidir: nlink'in abonelik ve ödeme servisi. Ödeme kuruluşu olarak PayTR kullanılır. Test edilmesini istediğimiz başlıklar:

- **Ödeme akışı:** Abonelik satın alma ve ödeme başlatma. Plan değişikliği ve fark ücreti hesabı.
- **Geri dönüşler:** PayTR geri dönüş (callback) uçlarının doğrulanması: imza/özet denetimi, tekrar oynatma (replay) ve sahte bildirim girişimleri.
- **Yenileme ve iptal:** Otomatik yenileme, başarısız ödeme ve iptal senaryoları.
- **Servisler arası çağrılar:** Ana uygulama (nlink API) ile ödeme servisi arasındaki iç çağrıların yetkilendirmesi.
- **Yetki ve veri:** Yetki aşımı (başka üyenin aboneliğine ya da ödeme kaydına erişim), girdi doğrulama, iş mantığı açıkları (tutar ya da plan değiştirme).
- **Bilgi sızıntısı:** Hata mesajları, kayıtlar (log) ve yanıtlarda gizli bilgi açığa çıkıyor mu.

Gerekirse kapsam, nlink web uygulaması ve API'sinin ödemeyle ilgili bölümlerine (plan sayfaları, hesap ve abonelik yönetimi) genişletilebilir. Bunu teklif aşamasında birlikte belirleyebiliriz.

## Test ortamı ve kurallar

- **Ortam:** Testler öncelikle **test ortamında** yapılır. Bu ortamda PayTR test modunda çalışır, gerçek kart çekimi yapılmaz.
- **Canlı ortam:** Canlı ortamda yapılacak kontroller yazılı onayla, sınırlı ve önceden belirlenmiş olur.
- **Yasak testler:** Hizmeti kesintiye uğratacak testler (DoS, yük testi) kapsam dışıdır. Gerçek kullanıcı verisine erişim ya da veri değiştirme yasaktır. Gerçek ödeme kartı kullanılmaz.
- **Kritik bulgu:** Test sırasında kritik bir açık bulunursa raporu beklemeden hemen bildirilmesini rica ederiz.
- **Gizlilik:** Bulgular ve erişim bilgileri gizli tutulur; yalnız taraflar arasında paylaşılır.

## Beklenen çıktılar

- Yönetici özeti ve teknik rapor: her bulgu için önem derecesi, etkilenen bileşen, adım adım yeniden üretme ve önerilen düzeltme.
- Düzeltmelerden sonra yeniden test ve kapanış notu.
- KOSGEB süreci için gerekli belgeler: fatura, rapor ve hizmet tamamlama belgeleri.

## İletişim

**Ramazan ERTAN** — nlink

İletişim bilgileri ve test ortamı erişimleri başlangıç toplantısında paylaşılacaktır.

Davetimizi değerlendirdiğiniz için teşekkür ederiz. Teklifinizi ve proforma faturanızı bekliyoruz.
