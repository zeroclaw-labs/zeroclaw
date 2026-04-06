import en from './en';
import zh from './zh';
import tr from './tr';
import es from './es';
import fr from './fr';
import de from './de';
import it from './it';
import pt from './pt';
import ru from './ru';
import ja from './ja';
import ko from './ko';
import ar from './ar';
import hi from './hi';
import nl from './nl';
import pl from './pl';
import sv from './sv';
import da from './da';
import nb from './no';
import fi from './fi';
import cs from './cs';
import ro from './ro';
import hu from './hu';
import el from './el';
import th from './th';
import vi from './vi';
import id from './id';
import ms from './ms';
import uk from './uk';
import he from './he';
import fil from './fil';
import bn from './bn';

export const translations: Record<string, Record<string, string>> = {
  en,
  zh,
  tr,
  es,
  fr,
  de,
  it,
  pt,
  ru,
  ja,
  ko,
  ar,
  hi,
  nl,
  pl,
  sv,
  da,
  no: nb,
  fi,
  cs,
  ro,
  hu,
  el,
  th,
  vi,
  id,
  ms,
  uk,
  he,
  fil,
  bn,
};

export const LOCALE_LIST: { code: string; name: string; nativeName: string }[] = [
  { code: 'en', name: 'English', nativeName: 'English' },
  { code: 'zh', name: 'Chinese', nativeName: '中文' },
  { code: 'tr', name: 'Turkish', nativeName: 'Türkçe' },
  { code: 'es', name: 'Spanish', nativeName: 'Español' },
  { code: 'fr', name: 'French', nativeName: 'Français' },
  { code: 'de', name: 'German', nativeName: 'Deutsch' },
  { code: 'it', name: 'Italian', nativeName: 'Italiano' },
  { code: 'pt', name: 'Portuguese', nativeName: 'Português' },
  { code: 'ru', name: 'Russian', nativeName: 'Русский' },
  { code: 'ja', name: 'Japanese', nativeName: '日本語' },
  { code: 'ko', name: 'Korean', nativeName: '한국어' },
  { code: 'ar', name: 'Arabic', nativeName: 'العربية' },
  { code: 'hi', name: 'Hindi', nativeName: 'हिन्दी' },
  { code: 'nl', name: 'Dutch', nativeName: 'Nederlands' },
  { code: 'pl', name: 'Polish', nativeName: 'Polski' },
  { code: 'sv', name: 'Swedish', nativeName: 'Svenska' },
  { code: 'da', name: 'Danish', nativeName: 'Dansk' },
  { code: 'no', name: 'Norwegian', nativeName: 'Norsk' },
  { code: 'fi', name: 'Finnish', nativeName: 'Suomi' },
  { code: 'cs', name: 'Czech', nativeName: 'Čeština' },
  { code: 'ro', name: 'Romanian', nativeName: 'Română' },
  { code: 'hu', name: 'Hungarian', nativeName: 'Magyar' },
  { code: 'el', name: 'Greek', nativeName: 'Ελληνικά' },
  { code: 'th', name: 'Thai', nativeName: 'ไทย' },
  { code: 'vi', name: 'Vietnamese', nativeName: 'Tiếng Việt' },
  { code: 'id', name: 'Indonesian', nativeName: 'Bahasa Indonesia' },
  { code: 'ms', name: 'Malay', nativeName: 'Bahasa Melayu' },
  { code: 'uk', name: 'Ukrainian', nativeName: 'Українська' },
  { code: 'he', name: 'Hebrew', nativeName: 'עברית' },
  { code: 'fil', name: 'Filipino', nativeName: 'Filipino' },
  { code: 'bn', name: 'Bengali', nativeName: 'বাংলা' },
];
