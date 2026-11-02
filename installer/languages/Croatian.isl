; *** Inno Setup version 6.5.0+ Croatian messages ***
; Translated by: Milo Ivir (mail@milotype.de)
; Based on translation by Elvis Gambiraža (el.gambo@gmail.com)
; Based on translation by Krunoslav Kanjuh (krunoslav.kanjuh@zg.t-com.hr)
;
; Source: https://raw.githubusercontent.com/jrsoftware/issrc/refs/heads/main/Files/Languages/Unofficial/Croatian.isl
; (user-contributed translations: https://www.jrsoftware.org/files/istrans/)
;
; Adapted for Dodavo: formal address ("Vi") as in Windows and the rest of the
; program, Windows terms ("Pregledaj", "pogreška", "izbornik Start"),
; Croatian quotation marks („…”) and button names that match the buttons
; actually shown.
;
; Note: When translating this text, do not add periods (.) to the end of
; messages that didn't have them already, because on those messages Inno
; Setup adds the periods automatically (appending a period would result in
; two periods being displayed).

[LangOptions]
; The following three entries are very important. Be sure to read and
; understand the '[LangOptions] section' topic in the help file.
LanguageName=Hrvatski
LanguageID=$041a
LanguageCodePage=1250
; If the language you are translating to requires special font faces or
; sizes, uncomment any of the following entries and change them accordingly.
;DialogFontName=MS Shell Dlg
;DialogFontSize=8
;WelcomeFontName=Arial
;WelcomeFontSize=12
;TitleFontName=Arial
;TitleFontSize=29
;CopyrightFontName=Arial
;CopyrightFontSize=8

[Messages]

; *** Application titles
SetupAppTitle=Instalacija
SetupWindowTitle=Instalacija – %1
UninstallAppTitle=Deinstalacija
UninstallAppFullTitle=Deinstalacija programa %1

; *** Misc. common
InformationTitle=Informacije
ConfirmTitle=Potvrda
ErrorTitle=Pogreška

; *** SetupLdr messages
SetupLdrStartupMessage=Instalirat će se %1. Želite li nastaviti?
LdrCannotCreateTemp=Nije moguće stvoriti privremenu datoteku. Instalacija je prekinuta
LdrCannotExecTemp=Nije moguće pokrenuti datoteku u privremenoj mapi. Instalacija je prekinuta
HelpTextNote=

; *** Startup error messages
LastErrorMessage=%1.%n%nPogreška %2: %3
SetupFileMissing=Datoteka %1 ne nalazi se u mapi instalacije. Ispravite problem ili nabavite novu kopiju programa.
SetupFileCorrupt=Datoteke instalacije su oštećene. Nabavite novu kopiju programa.
SetupFileCorruptOrWrongVer=Datoteke instalacije su oštećene ili nisu kompatibilne s ovom verzijom instalacije. Ispravite problem ili nabavite novu kopiju programa.
InvalidParameter=U naredbenom retku proslijeđen je neispravan parametar:%n%n%1
SetupAlreadyRunning=Instalacija je već pokrenuta.
WindowsVersionNotSupported=Program ne podržava verziju sustava Windows koju koristite.
WindowsServicePackRequired=Program zahtijeva %1 servisni paket %2 ili noviji.
NotOnThisPlatform=Program neće raditi na %1.
OnlyOnThisPlatform=Program se mora pokrenuti na %1.
OnlyOnTheseArchitectures=Program se može instalirati samo na verzijama sustava Windows za sljedeće procesorske arhitekture:%n%n%1
WinVersionTooLowError=Program zahtijeva %1 verziju %2 ili noviju.
WinVersionTooHighError=Program se ne može instalirati na %1 verziju %2 ili noviju.
AdminPrivilegesRequired=Za instaliranje programa morate biti prijavljeni kao administrator.
PowerUserPrivilegesRequired=Za instaliranje programa morate biti prijavljeni kao administrator ili kao član grupe naprednih korisnika.
SetupAppRunningError=Instalacija je otkrila da je %1 trenutačno pokrenut.%n%nZatvorite sve njegove instance pa kliknite „U redu” za nastavak ili „Odustani” za izlaz.
UninstallAppRunningError=Deinstalacija je otkrila da je %1 trenutačno pokrenut.%n%nZatvorite sve njegove instance pa kliknite „U redu” za nastavak ili „Odustani” za izlaz.

; *** Startup questions
PrivilegesRequiredOverrideTitle=Odabir načina instalacije
PrivilegesRequiredOverrideInstruction=Odaberite način instalacije
PrivilegesRequiredOverrideText1=%1 se može instalirati za sve korisnike (potrebna su administratorska prava) ili samo za vas.
PrivilegesRequiredOverrideText2=%1 se može instalirati samo za vas ili za sve korisnike (potrebna su administratorska prava).
PrivilegesRequiredOverrideAllUsers=Instaliraj z&a sve korisnike
PrivilegesRequiredOverrideAllUsersRecommended=Instaliraj z&a sve korisnike (preporučeno)
PrivilegesRequiredOverrideCurrentUser=Instaliraj samo za &mene
PrivilegesRequiredOverrideCurrentUserRecommended=Instaliraj samo za &mene (preporučeno)

; *** Misc. errors
ErrorCreatingDir=Instalacija nije mogla stvoriti mapu „%1”
ErrorTooManyFilesInDir=Nije moguće stvoriti datoteku u mapi „%1” jer mapa sadrži previše datoteka

; *** Setup common messages
ExitSetupTitle=Izlaz iz instalacije
ExitSetupMessage=Instalacija nije dovršena. Ako sada izađete, program se neće instalirati.%n%nInstalaciju možete ponovno pokrenuti i dovršiti neki drugi put.%n%nŽelite li izaći iz instalacije?
AboutSetupMenuItem=&O instalaciji…
AboutSetupTitle=O instalaciji
AboutSetupMessage=%1 verzija %2%n%3%n%n%1 web-stranica:%n%4
AboutSetupNote=
TranslatorNote=Prevoditelji:%n%nKrunoslav Kanjuh%n%nElvis Gambiraža%n%nMilo Ivir

; *** Buttons
ButtonBack=< Na&trag
ButtonNext=&Dalje >
ButtonInstall=&Instaliraj
ButtonOK=U redu
ButtonCancel=Odustani
ButtonYes=&Da
ButtonYesToAll=D&a za sve
ButtonNo=&Ne
ButtonNoToAll=N&e za sve
ButtonFinish=&Završi
ButtonBrowse=&Pregledaj…
ButtonWizardBrowse=P&regledaj…
ButtonNewFolder=&Stvori novu mapu

; *** "Select Language" dialog messages
SelectLanguageTitle=Odabir jezika instalacije
SelectLanguageLabel=Odaberite jezik koji želite koristiti tijekom instalacije.

; *** Common wizard text
ClickNext=Kliknite „Dalje” za nastavak ili „Odustani” za izlaz iz instalacije.
BeveledLabel=
BrowseDialogTitle=Odabir mape
BrowseDialogLabel=Odaberite mapu s popisa i kliknite „U redu”.
NewFolderName=Nova mapa

; *** "Welcome" wizard page
WelcomeLabel1=Dobro došli u instalaciju programa [name]
WelcomeLabel2=Na računalo će se instalirati [name/ver].%n%nPrije nastavka preporučujemo da zatvorite sve ostale programe.

; *** "Password" wizard page
WizardPassword=Lozinka
PasswordLabel1=Instalacija je zaštićena lozinkom.
PasswordLabel3=Unesite lozinku i kliknite „Dalje” za nastavak. Lozinke razlikuju mala i velika slova.
PasswordEditLabel=&Lozinka:
IncorrectPassword=Unesena lozinka nije ispravna. Pokušajte ponovno.

; *** "License Agreement" wizard page
WizardLicense=Licencni ugovor
LicenseLabel=Prije nastavka pažljivo pročitajte sljedeće važne informacije.
LicenseLabel3=Pročitajte sljedeći licencni ugovor. Za nastavak instalacije morate prihvatiti uvjete ugovora.
LicenseAccepted=&Prihvaćam ugovor
LicenseNotAccepted=&Ne prihvaćam ugovor

; *** "Information" wizard pages
WizardInfoBefore=Informacije
InfoBeforeLabel=Prije nastavka pažljivo pročitajte sljedeće važne informacije.
InfoBeforeClickLabel=Kada budete spremni za nastavak instalacije, kliknite „Dalje”.
WizardInfoAfter=Informacije
InfoAfterLabel=Prije nastavka pažljivo pročitajte sljedeće važne informacije.
InfoAfterClickLabel=Kada budete spremni za nastavak instalacije, kliknite „Dalje”.

; *** "User Information" wizard page
WizardUserInfo=Podaci o korisniku
UserInfoDesc=Unesite svoje podatke.
UserInfoName=&Ime korisnika:
UserInfoOrg=&Organizacija:
UserInfoSerial=&Serijski broj:
UserInfoNameRequired=Morate unijeti ime.

; *** "Select Destination Location" wizard page
WizardSelectDir=Odabir odredišta
SelectDirDesc=Gdje želite instalirati [name]?
SelectDirLabel3=[name] instalirat će se u sljedeću mapu.
SelectDirBrowseLabel=Za nastavak kliknite „Dalje”. Ako želite odabrati drugu mapu, kliknite „Pregledaj”.
DiskSpaceGBLabel=Potrebno je najmanje [gb] GB slobodnog prostora na disku.
DiskSpaceMBLabel=Potrebno je najmanje [mb] MB slobodnog prostora na disku.
CannotInstallToNetworkDrive=Instalacija ne može instalirati program na mrežni pogon.
CannotInstallToUNCPath=Instalacija ne može instalirati program na UNC put.
InvalidPath=Morate unijeti puni put sa slovom pogona, npr.:%n%nC:\APP%n%nili UNC put u obliku:%n%n\\poslužitelj\zajednički_resurs
InvalidDrive=Odabrani pogon ili UNC zajednički resurs ne postoji ili nije dostupan. Odaberite neki drugi.
DiskSpaceWarningTitle=Nema dovoljno prostora na disku
DiskSpaceWarning=Za instalaciju je potrebno najmanje %1 kB slobodnog prostora, a na odabranom pogonu dostupno je samo %2 kB.%n%nŽelite li ipak nastaviti?
DirNameTooLong=Naziv mape ili put je predugačak.
InvalidDirName=Naziv mape nije ispravan.
BadDirName32=Naziv mape ne smije sadržavati nijedan od sljedećih znakova:%n%n%1
DirExistsTitle=Mapa već postoji
DirExists=Mapa:%n%n%1%n%nveć postoji. Želite li ipak instalirati u nju?
DirDoesntExistTitle=Mapa ne postoji
DirDoesntExist=Mapa:%n%n%1%n%nne postoji. Želite li da se stvori?

; *** "Select Components" wizard page
WizardSelectComponents=Odabir komponenti
SelectComponentsDesc=Koje komponente želite instalirati?
SelectComponentsLabel2=Odaberite komponente koje želite instalirati, a poništite odabir onih koje ne želite instalirati. Kada budete spremni za nastavak, kliknite „Dalje”.
FullInstallation=Potpuna instalacija
; if possible don't translate 'Compact' as 'Minimal' (I mean 'Minimal' in your language)
CompactInstallation=Kompaktna instalacija
CustomInstallation=Prilagođena instalacija
NoUninstallWarningTitle=Komponente postoje
NoUninstallWarning=Instalacija je otkrila da su na računalu već instalirane sljedeće komponente:%n%n%1%n%nPoništavanjem odabira te se komponente neće deinstalirati.%n%nŽelite li ipak nastaviti?
ComponentSize1=%1 kB
ComponentSize2=%1 MB
ComponentsDiskSpaceGBLabel=Trenutačni odabir zahtijeva najmanje [gb] GB prostora na disku.
ComponentsDiskSpaceMBLabel=Trenutačni odabir zahtijeva najmanje [mb] MB prostora na disku.

; *** "Select Additional Tasks" wizard page
WizardSelectTasks=Odabir dodatnih zadataka
SelectTasksDesc=Koje dodatne zadatke treba izvršiti?
SelectTasksLabel2=Odaberite dodatne zadatke koje želite izvršiti tijekom instalacije programa [name], a zatim kliknite „Dalje”.

; *** "Select Start Menu Folder" wizard page
WizardSelectProgramGroup=Odabir mape na izborniku Start
SelectStartMenuFolderDesc=Gdje želite smjestiti prečace programa?
SelectStartMenuFolderLabel3=Instalacija će stvoriti prečace programa u sljedećoj mapi na izborniku Start.
SelectStartMenuFolderBrowseLabel=Za nastavak kliknite „Dalje”. Ako želite odabrati drugu mapu, kliknite „Pregledaj”.
MustEnterGroupName=Morate unijeti naziv mape.
GroupNameTooLong=Naziv mape ili put je predugačak.
InvalidGroupName=Naziv mape nije ispravan.
BadGroupName=Naziv mape ne smije sadržavati nijedan od sljedećih znakova:%n%n%1
NoProgramGroupCheck2=&Nemoj stvoriti mapu na izborniku Start

; *** "Ready to Install" wizard page
WizardReady=Spremno za instalaciju
ReadyLabel1=Instalacija je spremna za instaliranje programa [name] na računalo.
ReadyLabel2a=Kliknite „Instaliraj” za nastavak instalacije ili „Natrag” ako želite pregledati ili promijeniti postavke.
ReadyLabel2b=Kliknite „Instaliraj” za nastavak instalacije.
ReadyMemoUserInfo=Podaci o korisniku:
ReadyMemoDir=Odredišna mapa:
ReadyMemoType=Vrsta instalacije:
ReadyMemoComponents=Odabrane komponente:
ReadyMemoGroup=Mapa na izborniku Start:
ReadyMemoTasks=Dodatni zadaci:

; *** TDownloadWizardPage wizard page and DownloadTemporaryFile
DownloadingLabel2=Preuzimanje datoteka…
ButtonStopDownload=&Zaustavi preuzimanje
StopDownload=Jeste li sigurni da želite zaustaviti preuzimanje?
ErrorDownloadAborted=Preuzimanje je prekinuto
ErrorDownloadFailed=Preuzimanje nije uspjelo: %1 %2
ErrorDownloadSizeFailed=Dohvaćanje veličine nije uspjelo: %1 %2
ErrorProgress=Neispravan napredak: %1 od %2
ErrorFileSize=Neispravna veličina datoteke: očekivano %1, pronađeno %2

; *** TExtractionWizardPage wizard page and Extract7ZipArchive
ExtractingLabel=Raspakiravanje datoteka…
ButtonStopExtraction=&Zaustavi raspakiravanje
StopExtraction=Jeste li sigurni da želite zaustaviti raspakiravanje?
ErrorExtractionAborted=Raspakiravanje je prekinuto
ErrorExtractionFailed=Raspakiravanje nije uspjelo: %1

; *** Archive extraction failure details
ArchiveIncorrectPassword=Lozinka nije ispravna
ArchiveIsCorrupted=Arhiva je oštećena
ArchiveUnsupportedFormat=Format arhive nije podržan

; *** "Preparing to Install" wizard page
WizardPreparing=Priprema za instalaciju
PreparingDesc=Instalacija se priprema za instaliranje programa [name] na računalo.
PreviousInstallNotCompleted=Instalacija ili uklanjanje prethodnog programa nije dovršeno. Za dovršetak te instalacije morate ponovno pokrenuti računalo.%n%nNakon ponovnog pokretanja računala ponovno pokrenite instalaciju kako biste dovršili instaliranje programa [name].
CannotContinue=Instalacija ne može nastaviti. Kliknite „Odustani” za izlaz.
ApplicationsFound=Sljedeći programi koriste datoteke koje instalacija mora ažurirati. Preporučujemo da dopustite instalaciji da automatski zatvori te programe.
ApplicationsFound2=Sljedeći programi koriste datoteke koje instalacija mora ažurirati. Preporučujemo da dopustite instalaciji da automatski zatvori te programe. Kada instalacija završi, pokušat će ponovno pokrenuti programe.
CloseApplications=&Automatski zatvori programe
DontCloseApplications=&Nemoj zatvoriti programe
ErrorCloseApplications=Instalacija nije mogla automatski zatvoriti sve programe. Prije nastavka preporučujemo da zatvorite sve programe koji koriste datoteke koje instalacija mora ažurirati.
PrepareToInstallNeedsRestart=Instalacija mora ponovno pokrenuti računalo. Nakon ponovnog pokretanja računala ponovno pokrenite instalaciju kako biste dovršili instaliranje programa [name].%n%nŽelite li sada ponovno pokrenuti računalo?

; *** "Installing" wizard page
WizardInstalling=Instaliranje
InstallingLabel=Pričekajte dok se [name] instalira na računalo.

; *** "Setup Completed" wizard page
FinishedHeadingLabel=Dovršetak instalacije programa [name]
FinishedLabelNoIcons=Instalacija programa [name] na računalo je dovršena.
FinishedLabel=Instalacija programa [name] na računalo je dovršena. Program možete pokrenuti pomoću instaliranih prečaca.
ClickFinish=Kliknite „Završi” za izlaz iz instalacije.
FinishedRestartLabel=Za dovršetak instalacije programa [name] potrebno je ponovno pokrenuti računalo. Želite li ga sada ponovno pokrenuti?
FinishedRestartMessage=Za dovršetak instalacije programa [name] potrebno je ponovno pokrenuti računalo.%n%nŽelite li ga sada ponovno pokrenuti?
ShowReadmeCheck=Da, želim pročitati datoteku README
YesRadio=&Da, sada ponovno pokreni računalo
NoRadio=&Ne, računalo ću ponovno pokrenuti kasnije
; used for example as 'Run MyProg.exe'
RunEntryExec=Pokreni %1
; used for example as 'View Readme.txt'
RunEntryShellExec=Prikaži %1

; *** "Setup Needs the Next Disk" stuff
ChangeDiskTitle=Instalaciji je potreban sljedeći disk
SelectDiskLabel2=Umetnite disk %1 i kliknite „U redu”.%n%nAko se datoteke s ovog diska nalaze u mapi koja nije prikazana u nastavku, unesite ispravan put ili kliknite „Pregledaj”.
PathLabel=&Put:
FileNotInDir2=Datoteka „%1” ne nalazi se u „%2”. Umetnite ispravan disk ili odaberite drugu mapu.
SelectDirectoryLabel=Navedite mjesto sljedećeg diska.

; *** Installation phase messages
SetupAborted=Instalacija nije dovršena.%n%nIspravite problem i ponovno pokrenite instalaciju.
AbortRetryIgnoreSelectAction=Odaberite radnju
AbortRetryIgnoreRetry=&Pokušaj ponovno
AbortRetryIgnoreIgnore=&Zanemari pogrešku i nastavi
AbortRetryIgnoreCancel=Prekini instalaciju
RetryCancelSelectAction=Odaberite radnju
RetryCancelRetry=&Pokušaj ponovno
RetryCancelCancel=Odustani

; *** Installation status messages
StatusClosingApplications=Zatvaranje programa…
StatusCreateDirs=Stvaranje mapa…
StatusExtractFiles=Raspakiravanje datoteka…
StatusDownloadFiles=Preuzimanje datoteka…
StatusCreateIcons=Stvaranje prečaca…
StatusCreateIniEntries=Stvaranje INI unosa…
StatusCreateRegistryEntries=Stvaranje unosa u registru…
StatusRegisterFiles=Registriranje datoteka…
StatusSavingUninstall=Spremanje podataka za deinstalaciju…
StatusRunProgram=Dovršavanje instalacije…
StatusRestartingApplications=Ponovno pokretanje programa…
StatusRollback=Poništavanje promjena…

; *** Misc. errors
ErrorInternal2=Interna pogreška: %1
ErrorFunctionFailedNoCode=%1 – nije uspjelo
ErrorFunctionFailed=%1 – nije uspjelo; kôd %2
ErrorFunctionFailedWithMessage=%1 – nije uspjelo; kôd %2.%n%3
ErrorExecutingProgram=Nije moguće pokrenuti datoteku:%n%1

; *** Registry errors
ErrorRegOpenKey=Pogreška pri otvaranju ključa registra:%n%1\%2
ErrorRegCreateKey=Pogreška pri stvaranju ključa registra:%n%1\%2
ErrorRegWriteKey=Pogreška pri pisanju u ključ registra:%n%1\%2

; *** INI errors
ErrorIniEntry=Pogreška pri stvaranju INI unosa u datoteci „%1”.

; *** File copying errors
FileAbortRetryIgnoreSkipNotRecommended=&Preskoči ovu datoteku (ne preporučuje se)
FileAbortRetryIgnoreIgnoreNotRecommended=&Zanemari pogrešku i nastavi (ne preporučuje se)
SourceIsCorrupted=Izvorna datoteka je oštećena
SourceDoesntExist=Izvorna datoteka „%1” ne postoji
SourceVerificationFailed=Provjera izvorne datoteke nije uspjela: %1
VerificationSignatureDoesntExist=Datoteka potpisa „%1” ne postoji
VerificationSignatureInvalid=Datoteka potpisa „%1” nije valjana
VerificationKeyNotFound=Datoteka potpisa „%1” koristi nepoznati ključ
VerificationFileNameIncorrect=Naziv datoteke nije ispravan
VerificationFileTagIncorrect=Oznaka datoteke nije ispravna
VerificationFileSizeIncorrect=Veličina datoteke nije ispravna
VerificationFileHashIncorrect=Vrijednost raspršivanja datoteke nije ispravna
ExistingFileReadOnly2=Postojeću datoteku nije moguće zamijeniti jer je označena kao datoteka samo za čitanje.
ExistingFileReadOnlyRetry=&Ukloni atribut samo za čitanje i pokušaj ponovno
ExistingFileReadOnlyKeepExisting=&Zadrži postojeću datoteku
ErrorReadingExistingDest=Došlo je do pogreške pri čitanju postojeće datoteke:
FileExistsSelectAction=Odaberite radnju
FileExists2=Datoteka već postoji.
FileExistsOverwriteExisting=&Prebriši postojeću datoteku
FileExistsKeepExisting=&Zadrži postojeću datoteku
FileExistsOverwriteOrKeepAll=&Učini isto i za sljedeće sukobe
ExistingFileNewerSelectAction=Odaberite radnju
ExistingFileNewer2=Postojeća datoteka novija je od one koju instalacija pokušava instalirati.
ExistingFileNewerOverwriteExisting=&Prebriši postojeću datoteku
ExistingFileNewerKeepExisting=&Zadrži postojeću datoteku (preporučeno)
ExistingFileNewerOverwriteOrKeepAll=&Učini isto i za sljedeće sukobe
ErrorChangingAttr=Došlo je do pogreške pri promjeni atributa postojeće datoteke:
ErrorCreatingTemp=Došlo je do pogreške pri stvaranju datoteke u odredišnoj mapi:
ErrorReadingSource=Došlo je do pogreške pri čitanju izvorne datoteke:
ErrorCopying=Došlo je do pogreške pri kopiranju datoteke:
ErrorDownloading=Došlo je do pogreške pri preuzimanju datoteke:
ErrorExtracting=Došlo je do pogreške pri raspakiravanju arhive:
ErrorReplacingExistingFile=Došlo je do pogreške pri zamjeni postojeće datoteke:
ErrorRestartReplace=Zamjena nakon ponovnog pokretanja nije uspjela:
ErrorRenamingTemp=Došlo je do pogreške pri preimenovanju datoteke u odredišnoj mapi:
ErrorRegisterServer=Nije moguće registrirati DLL/OCX: %1
ErrorRegSvr32Failed=RegSvr32 nije uspio, izlazni kôd %1
ErrorRegisterTypeLib=Nije moguće registrirati biblioteku tipova: %1

; *** Uninstall display name markings
; used for example as 'My Program (32-bit)'
UninstallDisplayNameMark=%1 (%2)
; used for example as 'My Program (32-bit, All users)'
UninstallDisplayNameMarks=%1 (%2, %3)
UninstallDisplayNameMark32Bit=32-bitni
UninstallDisplayNameMark64Bit=64-bitni
UninstallDisplayNameMarkAllUsers=Svi korisnici
UninstallDisplayNameMarkCurrentUser=Trenutačni korisnik

; *** Post-installation errors
ErrorOpeningReadme=Došlo je do pogreške pri otvaranju datoteke README.
ErrorRestartingComputer=Instalacija nije mogla ponovno pokrenuti računalo. Učinite to ručno.

; *** Uninstaller messages
UninstallNotFound=Datoteka „%1” ne postoji. Deinstalacija nije moguća.
UninstallOpenError=Datoteku „%1” nije moguće otvoriti. Deinstalacija nije moguća
UninstallUnsupportedVer=Zapisnik deinstalacije „%1” u formatu je koji ova verzija programa za deinstalaciju ne prepoznaje. Deinstalacija nije moguća
UninstallUnknownEntry=U zapisniku deinstalacije pronađen je nepoznat unos (%1)
ConfirmUninstall=Jeste li sigurni da želite potpuno ukloniti %1 i sve njegove komponente?
UninstallOnlyOnWin64=Ova se instalacija može deinstalirati samo na 64-bitnom sustavu Windows.
OnlyAdminCanUninstall=Ovu instalaciju može deinstalirati samo korisnik s administratorskim pravima.
UninstallStatusLabel=Pričekajte dok se %1 uklanja s računala.
UninstalledAll=%1 uspješno je uklonjen s računala.
UninstalledMost=Deinstalacija programa %1 je dovršena.%n%nNeke elemente nije bilo moguće ukloniti. Možete ih ukloniti ručno.
UninstalledAndNeedsRestart=Za dovršetak deinstalacije programa %1 potrebno je ponovno pokrenuti računalo.%n%nŽelite li ga sada ponovno pokrenuti?
UninstallDataCorrupted=Datoteka „%1” je oštećena. Deinstalacija nije moguća

; *** Uninstallation phase messages
ConfirmDeleteSharedFileTitle=Ukloniti zajedničku datoteku?
ConfirmDeleteSharedFile2=Sustav pokazuje da sljedeću zajedničku datoteku više ne koristi nijedan program. Želite li da je deinstalacija ukloni?%n%nAko je neki program i dalje koristi, a ona se ukloni, taj program možda neće ispravno raditi. Ako niste sigurni, odaberite „Ne”. Ostavljanje datoteke na sustavu neće prouzročiti nikakvu štetu.
SharedFileNameLabel=Naziv datoteke:
SharedFileLocationLabel=Mjesto:
WizardUninstalling=Stanje deinstalacije
StatusUninstalling=Deinstaliranje programa %1…

; *** Shutdown block reasons
ShutdownBlockReasonInstallingApp=Instaliranje programa %1.
ShutdownBlockReasonUninstallingApp=Deinstaliranje programa %1.

; The custom messages below aren't used by Setup itself, but if you make
; use of them in your scripts, you'll want to translate them.

[CustomMessages]

NameAndVersion=%1 verzija %2
AdditionalIcons=Dodatni prečaci:
CreateDesktopIcon=Stvori prečac na ra&dnoj površini
CreateQuickLaunchIcon=Stvori prečac na traci za &brzo pokretanje
ProgramOnTheWeb=%1 na webu
UninstallProgram=Deinstaliraj %1
LaunchProgram=Pokreni %1
AssocFileExtension=&Poveži %1 s nastavkom naziva datoteke %2
AssocingFileExtension=Povezivanje programa %1 s nastavkom naziva datoteke %2…
AutoStartProgramGroupDescription=Pokretanje:
AutoStartProgram=Automatski pokreni %1
AddonHostProgramNotFound=%1 nije pronađen u odabranoj mapi.%n%nŽelite li ipak nastaviti?
