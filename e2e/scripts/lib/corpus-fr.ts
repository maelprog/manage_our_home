//! Le corpus français du seed de performance.
//!
//! Pourquoi du vrai texte de foyer et pas `article 1`, `article 2` : le poids
//! des pages est mesuré **compressé**, et gzip/zstd écrasent des libellés
//! répétitifs à presque rien. Un corpus artificiel donnerait donc un budget
//! optimiste — exactement le genre de chiffre faux que ces scripts existent
//! pour éviter. Les entrées sont distinctes, de longueur variable, et
//! accentuées (l'UTF-8 multi-octets compte dans les octets mesurés).
//!
//! Ces listes sont volontairement plus longues que les cibles du seed : on en
//! consomme un préfixe, ce qui garde le contenu identique quand la cible
//! change.

export type EventSeed = { title: string; description: string; location: string };

export const EVENTS: readonly EventSeed[] = [
  {
    title: "Rendez-vous pédiatre — Léa",
    description: "Visite des 6 ans, penser au carnet de santé et à la carte Vitale.",
    location: "Cabinet médical, 12 rue des Lilas",
  },
  {
    title: "Réunion parents-professeurs",
    description: "Bulletin du premier trimestre, prévoir 20 minutes par enseignant.",
    location: "Collège Jean-Moulin",
  },
  {
    title: "Contrôle technique de la voiture",
    description: "Expire à la fin du mois — apporter la carte grise.",
    location: "Centre auto, zone des Prés",
  },
  {
    title: "Cours de piano de Nino",
    description: "Répéter le morceau de la semaine avant d'y aller.",
    location: "Conservatoire municipal",
  },
  {
    title: "Sortie scolaire au musée",
    description: "Pique-nique à préparer la veille, autorisation signée déjà rendue.",
    location: "Musée d'histoire naturelle",
  },
  {
    title: "Livraison du lave-vaisselle",
    description: "Créneau annoncé entre 8 h et 12 h, quelqu'un doit être là.",
    location: "Maison",
  },
  {
    title: "Sortir les poubelles jaunes",
    description: "Collecte du tri, à sortir la veille au soir.",
    location: "Trottoir",
  },
  {
    title: "Anniversaire de mamie Suzanne",
    description: "Gâteau commandé chez le pâtissier, à récupérer le matin.",
    location: "Chez Suzanne",
  },
  {
    title: "Vaccin du chat chez le vétérinaire",
    description: "Rappel annuel, prendre la cage de transport.",
    location: "Clinique vétérinaire des Tilleuls",
  },
  {
    title: "Réunion de copropriété",
    description: "Ordre du jour : ravalement de façade et budget de l'ascenseur.",
    location: "Salle communale",
  },
  {
    title: "Match de foot de Nino",
    description: "Prévoir les crampons et une bouteille d'eau.",
    location: "Stade municipal, terrain 2",
  },
  {
    title: "Passer à la déchetterie",
    description: "Cartons du déménagement et vieux pots de peinture.",
    location: "Déchetterie intercommunale",
  },
  {
    title: "Dentiste — détartrage",
    description: "Penser à demander la feuille de soins pour la mutuelle.",
    location: "Cabinet dentaire, place du Marché",
  },
  {
    title: "Courses de la semaine",
    description: "Faire la liste à partir des recettes prévues et des stocks bas.",
    location: "Supermarché du centre",
  },
  {
    title: "Relevé du compteur d'eau",
    description: "Noter l'index et le transmettre au syndic avant la fin du mois.",
    location: "Cave",
  },
  {
    title: "Kiné pour le dos",
    description: "Séance 4 sur 10, apporter l'ordonnance.",
    location: "Cabinet de kinésithérapie",
  },
  {
    title: "Repas de famille chez les parents",
    description: "Apporter le dessert et une bouteille de cidre.",
    location: "Chez Michel et Annie",
  },
  {
    title: "Réviser la chaudière",
    description: "Entretien annuel obligatoire, contrat avec le chauffagiste.",
    location: "Maison",
  },
  {
    title: "Bibliothèque — retour des livres",
    description: "Trois romans et deux albums, éviter les pénalités de retard.",
    location: "Médiathèque",
  },
  {
    title: "Cours de natation de Léa",
    description: "Bonnet de bain obligatoire, arriver 10 minutes en avance.",
    location: "Piscine Aquavive",
  },
  {
    title: "Rendez-vous banque — prêt travaux",
    description: "Apporter les devis de l'artisan et les trois derniers relevés.",
    location: "Agence bancaire, avenue de la Gare",
  },
  {
    title: "Nettoyage de printemps du garage",
    description: "Trier les outils, jeter ce qui ne sert plus depuis deux ans.",
    location: "Garage",
  },
  {
    title: "Vidange et pneus",
    description: "Demander aussi un devis pour les plaquettes de frein.",
    location: "Garage Renaud",
  },
  {
    title: "Spectacle de fin d'année de l'école",
    description: "Salle ouverte 30 minutes avant, places non numérotées.",
    location: "Salle des fêtes",
  },
  {
    title: "Appeler la mutuelle",
    description: "Contester le remboursement des lunettes de Léa.",
    location: "Téléphone",
  },
  {
    title: "Élaguer la haie du fond",
    description: "Avant que les oiseaux ne nichent, prévoir le taille-haie.",
    location: "Jardin",
  },
  {
    title: "Réunion de rentrée du club de judo",
    description: "Inscription à renouveler, certificat médical à fournir.",
    location: "Dojo municipal",
  },
  {
    title: "Récupérer le colis au relais",
    description: "Dernier jour de retrait, penser à la pièce d'identité.",
    location: "Relais colis, tabac de la place",
  },
  {
    title: "Soirée jeux de société",
    description: "Les voisins viennent avec les enfants, prévoir de quoi grignoter.",
    location: "Salon",
  },
  {
    title: "Déclarer les revenus en ligne",
    description: "Vérifier les frais de garde et les dons avant de valider.",
    location: "Bureau",
  },
  {
    title: "Rendez-vous ophtalmo",
    description: "Contrôle annuel, la vue de Nino baisse un peu.",
    location: "Centre ophtalmologique",
  },
  {
    title: "Changer les filtres de la VMC",
    description: "Modèle à racheter en magasin de bricolage.",
    location: "Salle de bain",
  },
  {
    title: "Brocante du quartier",
    description: "Vendre les vêtements devenus trop petits et les vieux jouets.",
    location: "Place de la Liberté",
  },
  {
    title: "Goûter d'anniversaire de Léa",
    description: "Huit copains de classe, prévoir gâteau, jus et une activité.",
    location: "Maison",
  },
  {
    title: "Réserver les vacances d'été",
    description: "Comparer le gîte en Bretagne et le camping dans le Jura.",
    location: "Bureau",
  },
  {
    title: "Contrôle de la caméra de la porte",
    description: "Batterie faible signalée depuis une semaine.",
    location: "Entrée",
  },
  {
    title: "Rendez-vous coiffeur",
    description: "Coupe pour toute la famille, prévoir une heure et demie.",
    location: "Salon Éclat",
  },
  {
    title: "Atelier compost de la mairie",
    description: "Inscription gratuite, un composteur offert par foyer.",
    location: "Jardins partagés",
  },
  {
    title: "Réviser les vélos avant les beaux jours",
    description: "Pneus, freins et chaîne — kit de réparation à racheter.",
    location: "Garage",
  },
  {
    title: "Visite du terrain avec l'architecte",
    description: "Discuter de l'extension côté jardin et de l'orientation.",
    location: "Terrain, chemin des Sources",
  },
  {
    title: "Rendez-vous orthophoniste",
    description: "Bilan de fin de suivi, apporter le cahier d'exercices.",
    location: "Cabinet, rue Pasteur",
  },
  {
    title: "Nettoyer les gouttières",
    description: "Après la chute des feuilles, prévoir l'échelle du voisin.",
    location: "Toiture",
  },
  {
    title: "Réunion du conseil d'école",
    description: "Représentant des parents, ordre du jour envoyé par mail.",
    location: "École élémentaire des Chênes",
  },
  {
    title: "Livraison du bois de chauffage",
    description: "Deux stères, prévoir de la place sous l'auvent.",
    location: "Cour",
  },
];

export type StockSeed = {
  name: string;
  category: string;
  unit: string;
  quantity: number;
  reorderThreshold: number;
};

export const STOCK_ITEMS: readonly StockSeed[] = [
  { name: "Farine de blé T65", category: "Épicerie sèche", unit: "kg", quantity: 2, reorderThreshold: 1 },
  { name: "Sucre en poudre", category: "Épicerie sèche", unit: "kg", quantity: 1.5, reorderThreshold: 0.5 },
  { name: "Riz basmati", category: "Épicerie sèche", unit: "kg", quantity: 3, reorderThreshold: 1 },
  { name: "Pâtes penne complètes", category: "Épicerie sèche", unit: "paquet", quantity: 4, reorderThreshold: 2 },
  { name: "Lentilles vertes du Puy", category: "Épicerie sèche", unit: "kg", quantity: 1, reorderThreshold: 0.5 },
  { name: "Pois chiches secs", category: "Épicerie sèche", unit: "kg", quantity: 0.8, reorderThreshold: 0.5 },
  { name: "Huile d'olive vierge extra", category: "Épicerie sèche", unit: "L", quantity: 2, reorderThreshold: 1 },
  { name: "Vinaigre de cidre", category: "Épicerie sèche", unit: "L", quantity: 1, reorderThreshold: 0.5 },
  { name: "Sel de Guérande", category: "Épicerie sèche", unit: "kg", quantity: 1, reorderThreshold: 0.25 },
  { name: "Poivre noir en grains", category: "Épicerie sèche", unit: "g", quantity: 120, reorderThreshold: 40 },
  { name: "Café en grains", category: "Petit-déjeuner", unit: "kg", quantity: 1, reorderThreshold: 0.5 },
  { name: "Thé vert en vrac", category: "Petit-déjeuner", unit: "g", quantity: 200, reorderThreshold: 80 },
  { name: "Confiture d'abricot", category: "Petit-déjeuner", unit: "pot", quantity: 3, reorderThreshold: 1 },
  { name: "Miel de châtaignier", category: "Petit-déjeuner", unit: "pot", quantity: 2, reorderThreshold: 1 },
  { name: "Flocons d'avoine", category: "Petit-déjeuner", unit: "kg", quantity: 1.2, reorderThreshold: 0.5 },
  { name: "Lait demi-écrémé", category: "Frais", unit: "L", quantity: 6, reorderThreshold: 3 },
  { name: "Beurre demi-sel", category: "Frais", unit: "plaquette", quantity: 2, reorderThreshold: 1 },
  { name: "Œufs plein air", category: "Frais", unit: "boîte", quantity: 2, reorderThreshold: 1 },
  { name: "Yaourts nature", category: "Frais", unit: "pot", quantity: 12, reorderThreshold: 6 },
  { name: "Comté 18 mois", category: "Frais", unit: "g", quantity: 400, reorderThreshold: 150 },
  { name: "Crème fraîche épaisse", category: "Frais", unit: "pot", quantity: 2, reorderThreshold: 1 },
  { name: "Jambon blanc", category: "Frais", unit: "tranche", quantity: 6, reorderThreshold: 4 },
  { name: "Épinards surgelés", category: "Surgelés", unit: "kg", quantity: 1, reorderThreshold: 0.5 },
  { name: "Petits pois surgelés", category: "Surgelés", unit: "kg", quantity: 1.5, reorderThreshold: 0.5 },
  { name: "Filets de cabillaud", category: "Surgelés", unit: "portion", quantity: 4, reorderThreshold: 2 },
  { name: "Glace vanille", category: "Surgelés", unit: "bac", quantity: 1, reorderThreshold: 1 },
  { name: "Tomates pelées", category: "Conserves", unit: "boîte", quantity: 5, reorderThreshold: 2 },
  { name: "Thon au naturel", category: "Conserves", unit: "boîte", quantity: 4, reorderThreshold: 2 },
  { name: "Haricots verts extra-fins", category: "Conserves", unit: "bocal", quantity: 3, reorderThreshold: 1 },
  { name: "Maïs doux", category: "Conserves", unit: "boîte", quantity: 2, reorderThreshold: 1 },
  { name: "Liquide vaisselle", category: "Entretien", unit: "flacon", quantity: 2, reorderThreshold: 1 },
  { name: "Lessive écologique", category: "Entretien", unit: "L", quantity: 3, reorderThreshold: 1 },
  { name: "Éponges grattantes", category: "Entretien", unit: "unité", quantity: 6, reorderThreshold: 2 },
  { name: "Sacs poubelle 30 L", category: "Entretien", unit: "rouleau", quantity: 2, reorderThreshold: 1 },
  { name: "Papier toilette", category: "Entretien", unit: "rouleau", quantity: 12, reorderThreshold: 6 },
  { name: "Vinaigre blanc ménager", category: "Entretien", unit: "L", quantity: 2, reorderThreshold: 1 },
  { name: "Dentifrice menthe", category: "Hygiène", unit: "tube", quantity: 3, reorderThreshold: 1 },
  { name: "Gel douche familial", category: "Hygiène", unit: "flacon", quantity: 2, reorderThreshold: 1 },
  { name: "Shampoing doux", category: "Hygiène", unit: "flacon", quantity: 1, reorderThreshold: 1 },
  { name: "Mouchoirs en papier", category: "Hygiène", unit: "boîte", quantity: 4, reorderThreshold: 2 },
  { name: "Pansements assortis", category: "Pharmacie", unit: "boîte", quantity: 2, reorderThreshold: 1 },
  { name: "Paracétamol 500 mg", category: "Pharmacie", unit: "boîte", quantity: 2, reorderThreshold: 1 },
  { name: "Sérum physiologique", category: "Pharmacie", unit: "dosette", quantity: 20, reorderThreshold: 10 },
  { name: "Piles LR6", category: "Divers", unit: "unité", quantity: 8, reorderThreshold: 4 },
  { name: "Ampoules LED E27", category: "Divers", unit: "unité", quantity: 3, reorderThreshold: 2 },
];

export type GrocerySeed = { name: string; quantity: number; unit: string };

export const GROCERY_ITEMS: readonly GrocerySeed[] = [
  { name: "Pain de campagne au levain", quantity: 1, unit: "miche" },
  { name: "Bananes bio", quantity: 1.2, unit: "kg" },
  { name: "Pommes Chantecler", quantity: 2, unit: "kg" },
  { name: "Carottes fanes", quantity: 1, unit: "botte" },
  { name: "Poireaux", quantity: 3, unit: "unité" },
  { name: "Oignons jaunes", quantity: 1.5, unit: "kg" },
  { name: "Ail rose de Lautrec", quantity: 2, unit: "tête" },
  { name: "Salade batavia", quantity: 1, unit: "unité" },
  { name: "Tomates grappe", quantity: 1, unit: "kg" },
  { name: "Courgettes", quantity: 4, unit: "unité" },
  { name: "Pommes de terre à chair ferme", quantity: 3, unit: "kg" },
  { name: "Champignons de Paris", quantity: 500, unit: "g" },
  { name: "Blanc de poulet fermier", quantity: 800, unit: "g" },
  { name: "Steak haché 5 %", quantity: 6, unit: "unité" },
  { name: "Saumon frais", quantity: 400, unit: "g" },
  { name: "Emmental râpé", quantity: 200, unit: "g" },
  { name: "Chèvre frais", quantity: 2, unit: "bûche" },
  { name: "Baguette tradition", quantity: 2, unit: "unité" },
  { name: "Croissants du dimanche", quantity: 4, unit: "unité" },
  { name: "Jus d'orange pressé", quantity: 2, unit: "L" },
  { name: "Eau pétillante", quantity: 6, unit: "bouteille" },
  { name: "Chocolat noir pâtissier", quantity: 2, unit: "tablette" },
  { name: "Biscuits pour le goûter", quantity: 2, unit: "paquet" },
  { name: "Céréales complètes", quantity: 1, unit: "paquet" },
  { name: "Basilic frais", quantity: 1, unit: "pot" },
  { name: "Persil plat", quantity: 1, unit: "botte" },
  { name: "Citrons jaunes", quantity: 4, unit: "unité" },
  { name: "Olives vertes dénoyautées", quantity: 1, unit: "bocal" },
  { name: "Moutarde de Dijon", quantity: 1, unit: "pot" },
  { name: "Sopalin", quantity: 4, unit: "rouleau" },
  { name: "Croquettes pour le chat", quantity: 2, unit: "kg" },
  { name: "Litière végétale", quantity: 1, unit: "sac" },
  { name: "Fromage blanc 20 %", quantity: 1, unit: "pot" },
  { name: "Pâte brisée", quantity: 2, unit: "rouleau" },
  { name: "Levure de boulanger", quantity: 3, unit: "sachet" },
];

export type BudgetSeed = { name: string; amount: number };

export const BUDGET_ENTRIES: readonly BudgetSeed[] = [
  { name: "Courses hebdomadaires au supermarché", amount: 142.37 },
  { name: "Marché du samedi — fruits et légumes", amount: 38.9 },
  { name: "Boulangerie de la semaine", amount: 17.4 },
  { name: "Plein d'essence", amount: 78.2 },
  { name: "Abonnement transports en commun", amount: 75 },
  { name: "Cantine scolaire de Léa", amount: 96.5 },
  { name: "Cantine scolaire de Nino", amount: 96.5 },
  { name: "Facture d'électricité", amount: 118.64 },
  { name: "Facture de gaz", amount: 64.12 },
  { name: "Abonnement internet et mobile", amount: 49.99 },
  { name: "Assurance habitation (mensualité)", amount: 31.8 },
  { name: "Assurance auto (mensualité)", amount: 54.25 },
  { name: "Mutuelle santé", amount: 128.4 },
  { name: "Pharmacie — ordonnance du pédiatre", amount: 23.15 },
  { name: "Consultation dentiste", amount: 42 },
  { name: "Cours de piano (trimestre)", amount: 165 },
  { name: "Licence de judo", amount: 87 },
  { name: "Sortie scolaire au musée", amount: 12 },
  { name: "Livres et fournitures scolaires", amount: 46.7 },
  { name: "Vêtements d'hiver pour les enfants", amount: 134.9 },
  { name: "Chaussures de sport", amount: 59.99 },
  { name: "Coiffeur (famille)", amount: 68 },
  { name: "Restaurant en famille", amount: 92.3 },
  { name: "Cinéma du dimanche", amount: 34 },
  { name: "Croquettes et litière du chat", amount: 41.6 },
  { name: "Vétérinaire — rappel de vaccin", amount: 62 },
  { name: "Quincaillerie — visserie et joints", amount: 27.85 },
  { name: "Terreau et plants pour le potager", amount: 44.3 },
  { name: "Entretien de la chaudière", amount: 149 },
  { name: "Vidange et filtres de la voiture", amount: 186.4 },
  { name: "Abonnement médiathèque", amount: 22 },
  { name: "Cadeau d'anniversaire (mamie Suzanne)", amount: 55 },
  { name: "Produits d'entretien", amount: 33.75 },
  { name: "Taxe d'ordures ménagères (mensualisée)", amount: 29.5 },
  { name: "Épargne de précaution", amount: 200 },
];

export type RecipeSeed = {
  name: string;
  instructions: string;
  ingredients: { name: string; quantity: number; unit: string }[];
};

export const RECIPES: readonly RecipeSeed[] = [
  {
    name: "Gratin dauphinois",
    instructions:
      "Éplucher et couper les pommes de terre en fines rondelles. Frotter le plat avec une gousse d'ail. Alterner les couches de pommes de terre, sel, poivre et muscade. Verser la crème et le lait à hauteur. Enfourner 1 h 15 à 160 °C, jusqu'à ce que la lame d'un couteau traverse sans résistance.",
    ingredients: [
      { name: "Pommes de terre à chair ferme", quantity: 1.2, unit: "kg" },
      { name: "Crème fraîche épaisse", quantity: 25, unit: "cl" },
      { name: "Lait demi-écrémé", quantity: 25, unit: "cl" },
      { name: "Ail", quantity: 1, unit: "gousse" },
    ],
  },
  {
    name: "Soupe de potimarron au lait de coco",
    instructions:
      "Faire revenir l'oignon, ajouter le potimarron en cubes et couvrir de bouillon. Laisser mijoter 25 minutes, mixer, puis détendre avec le lait de coco. Rectifier l'assaisonnement avec une pointe de curry.",
    ingredients: [
      { name: "Potimarron", quantity: 1, unit: "unité" },
      { name: "Oignon jaune", quantity: 1, unit: "unité" },
      { name: "Lait de coco", quantity: 20, unit: "cl" },
    ],
  },
  {
    name: "Poulet basquaise",
    instructions:
      "Colorer les cuisses de poulet, réserver. Faire fondre poivrons et oignons, ajouter les tomates et le piment d'Espelette. Remettre le poulet, couvrir et laisser mijoter 40 minutes. Servir avec du riz.",
    ingredients: [
      { name: "Cuisses de poulet", quantity: 4, unit: "unité" },
      { name: "Poivrons rouges", quantity: 2, unit: "unité" },
      { name: "Tomates pelées", quantity: 1, unit: "boîte" },
      { name: "Riz basmati", quantity: 300, unit: "g" },
    ],
  },
  {
    name: "Quiche aux poireaux et au comté",
    instructions:
      "Émincer les poireaux et les faire fondre au beurre 15 minutes. Étaler la pâte, répartir les poireaux et le comté râpé. Verser l'appareil œufs-crème, cuire 35 minutes à 180 °C.",
    ingredients: [
      { name: "Pâte brisée", quantity: 1, unit: "rouleau" },
      { name: "Poireaux", quantity: 3, unit: "unité" },
      { name: "Comté", quantity: 150, unit: "g" },
      { name: "Œufs", quantity: 3, unit: "unité" },
    ],
  },
  {
    name: "Lentilles aux saucisses de Morteau",
    instructions:
      "Rincer les lentilles, les couvrir d'eau froide avec carotte, oignon piqué de clous de girofle et bouquet garni. Cuire 30 minutes, ajouter les saucisses pochées à part, laisser reposer 10 minutes avant de servir.",
    ingredients: [
      { name: "Lentilles vertes du Puy", quantity: 400, unit: "g" },
      { name: "Saucisse de Morteau", quantity: 2, unit: "unité" },
      { name: "Carottes", quantity: 2, unit: "unité" },
    ],
  },
  {
    name: "Ratatouille du dimanche",
    instructions:
      "Cuire chaque légume séparément à l'huile d'olive pour garder les textures, puis réunir avec l'ail et le thym. Laisser compoter à couvert 30 minutes. Meilleure réchauffée le lendemain.",
    ingredients: [
      { name: "Courgettes", quantity: 3, unit: "unité" },
      { name: "Aubergines", quantity: 2, unit: "unité" },
      { name: "Poivrons", quantity: 2, unit: "unité" },
      { name: "Tomates", quantity: 6, unit: "unité" },
    ],
  },
  {
    name: "Hachis parmentier maison",
    instructions:
      "Préparer une purée bien beurrée. Faire revenir la viande hachée avec oignon et persil, déglacer au vin blanc. Monter en couches dans un plat, parsemer de fromage râpé et gratiner 20 minutes.",
    ingredients: [
      { name: "Pommes de terre", quantity: 1, unit: "kg" },
      { name: "Steak haché", quantity: 500, unit: "g" },
      { name: "Emmental râpé", quantity: 100, unit: "g" },
    ],
  },
  {
    name: "Salade de lentilles au chèvre frais",
    instructions:
      "Cuire les lentilles al dente, les rafraîchir. Mélanger avec échalote ciselée, vinaigrette à la moutarde, et émietter le chèvre frais au dernier moment.",
    ingredients: [
      { name: "Lentilles vertes du Puy", quantity: 250, unit: "g" },
      { name: "Chèvre frais", quantity: 1, unit: "bûche" },
      { name: "Moutarde de Dijon", quantity: 1, unit: "cuillère" },
    ],
  },
  {
    name: "Blanquette de veau à l'ancienne",
    instructions:
      "Blanchir la viande, la cuire 1 h 30 avec carottes, poireau et bouquet garni. Préparer un roux avec le bouillon filtré, lier avec crème et jaune d'œuf hors du feu. Ajouter champignons et petits oignons.",
    ingredients: [
      { name: "Épaule de veau", quantity: 1, unit: "kg" },
      { name: "Champignons de Paris", quantity: 250, unit: "g" },
      { name: "Crème fraîche épaisse", quantity: 20, unit: "cl" },
    ],
  },
  {
    name: "Tarte aux pommes de mamie",
    instructions:
      "Étaler la pâte, piquer le fond, saupoudrer de poudre d'amandes. Ranger les lamelles de pommes en rosace, sucrer et parsemer de noisettes de beurre. Cuire 40 minutes à 180 °C, napper de confiture d'abricot tiède.",
    ingredients: [
      { name: "Pommes Chantecler", quantity: 5, unit: "unité" },
      { name: "Pâte brisée", quantity: 1, unit: "rouleau" },
      { name: "Confiture d'abricot", quantity: 2, unit: "cuillère" },
    ],
  },
  {
    name: "Curry de pois chiches aux épinards",
    instructions:
      "Faire revenir oignon, ail et gingembre, ajouter les épices et les pois chiches égouttés. Mouiller avec le lait de coco, laisser réduire 20 minutes, incorporer les épinards en fin de cuisson.",
    ingredients: [
      { name: "Pois chiches", quantity: 500, unit: "g" },
      { name: "Épinards surgelés", quantity: 300, unit: "g" },
      { name: "Lait de coco", quantity: 20, unit: "cl" },
    ],
  },
  {
    name: "Croque-monsieur au four",
    instructions:
      "Préparer une béchamel épaisse. Tartiner le pain de mie, garnir de jambon et de comté, refermer, napper de béchamel et de fromage. Enfourner 15 minutes à 200 °C puis 2 minutes sous le gril.",
    ingredients: [
      { name: "Pain de mie", quantity: 8, unit: "tranche" },
      { name: "Jambon blanc", quantity: 4, unit: "tranche" },
      { name: "Comté", quantity: 120, unit: "g" },
    ],
  },
  {
    name: "Cabillaud au four et petits légumes",
    instructions:
      "Disposer les filets sur un lit de courgettes et de tomates cerises, arroser d'huile d'olive et de citron. Cuire 18 minutes à 180 °C, parsemer de persil plat au moment de servir.",
    ingredients: [
      { name: "Filets de cabillaud", quantity: 4, unit: "portion" },
      { name: "Courgettes", quantity: 2, unit: "unité" },
      { name: "Citron jaune", quantity: 1, unit: "unité" },
    ],
  },
  {
    name: "Risotto aux champignons",
    instructions:
      "Nacrer le riz, déglacer au vin blanc, puis mouiller louche par louche avec le bouillon chaud. Ajouter les champignons poêlés à mi-cuisson. Hors du feu, monter au beurre et au parmesan.",
    ingredients: [
      { name: "Riz arborio", quantity: 320, unit: "g" },
      { name: "Champignons de Paris", quantity: 300, unit: "g" },
      { name: "Parmesan", quantity: 80, unit: "g" },
    ],
  },
  {
    name: "Chili sin carne",
    instructions:
      "Faire revenir oignon, poivron et ail, ajouter les épices, les haricots rouges, le maïs et les tomates. Laisser mijoter 35 minutes à découvert. Servir avec du riz et un peu de fromage blanc.",
    ingredients: [
      { name: "Haricots rouges", quantity: 2, unit: "boîte" },
      { name: "Maïs doux", quantity: 1, unit: "boîte" },
      { name: "Tomates pelées", quantity: 1, unit: "boîte" },
    ],
  },
  {
    name: "Omelette aux herbes du jardin",
    instructions:
      "Battre les œufs sans excès, saler. Verser dans la poêle bien chaude, ramener les bords au centre, ajouter les herbes ciselées et rouler l'omelette encore baveuse.",
    ingredients: [
      { name: "Œufs plein air", quantity: 6, unit: "unité" },
      { name: "Persil plat", quantity: 0.5, unit: "botte" },
      { name: "Beurre demi-sel", quantity: 20, unit: "g" },
    ],
  },
  {
    name: "Pâtes au pesto de basilic",
    instructions:
      "Piler basilic, pignons, ail et parmesan, monter à l'huile d'olive. Détendre le pesto avec un peu d'eau de cuisson des pâtes avant de mélanger, hors du feu.",
    ingredients: [
      { name: "Pâtes penne complètes", quantity: 400, unit: "g" },
      { name: "Basilic frais", quantity: 1, unit: "pot" },
      { name: "Parmesan", quantity: 60, unit: "g" },
    ],
  },
  {
    name: "Rôti de porc aux pruneaux",
    instructions:
      "Saisir le rôti sur toutes ses faces, ajouter les échalotes et les pruneaux, déglacer au vin blanc. Cuire à couvert 1 h en arrosant régulièrement. Laisser reposer avant de trancher.",
    ingredients: [
      { name: "Rôti de porc", quantity: 1, unit: "kg" },
      { name: "Pruneaux d'Agen", quantity: 200, unit: "g" },
      { name: "Échalotes", quantity: 4, unit: "unité" },
    ],
  },
  {
    name: "Crêpes du mardi soir",
    instructions:
      "Mélanger farine, œufs et lait sans grumeaux, ajouter une cuillère d'huile et laisser reposer une heure. Cuire à feu vif dans une poêle à peine graissée.",
    ingredients: [
      { name: "Farine de blé T65", quantity: 250, unit: "g" },
      { name: "Œufs plein air", quantity: 3, unit: "unité" },
      { name: "Lait demi-écrémé", quantity: 50, unit: "cl" },
    ],
  },
  {
    name: "Soupe à l'oignon gratinée",
    instructions:
      "Faire compoter les oignons émincés 40 minutes à feu doux jusqu'à belle coloration. Mouiller au bouillon, laisser frémir 20 minutes. Gratiner avec une tranche de pain et du comté râpé.",
    ingredients: [
      { name: "Oignons jaunes", quantity: 800, unit: "g" },
      { name: "Comté", quantity: 100, unit: "g" },
      { name: "Pain de campagne au levain", quantity: 4, unit: "tranche" },
    ],
  },
  {
    name: "Salade de riz complète",
    instructions:
      "Cuire le riz, le rafraîchir. Ajouter thon, maïs, tomates, œufs durs et olives. Assaisonner à la vinaigrette au vinaigre de cidre et laisser reposer au frais une heure.",
    ingredients: [
      { name: "Riz basmati", quantity: 300, unit: "g" },
      { name: "Thon au naturel", quantity: 2, unit: "boîte" },
      { name: "Maïs doux", quantity: 1, unit: "boîte" },
    ],
  },
  {
    name: "Pot-au-feu d'hiver",
    instructions:
      "Couvrir la viande d'eau froide, écumer, puis ajouter les légumes et le bouquet garni. Laisser frémir 3 heures sans jamais bouillir. Servir avec moutarde et cornichons.",
    ingredients: [
      { name: "Paleron de bœuf", quantity: 1.2, unit: "kg" },
      { name: "Carottes", quantity: 6, unit: "unité" },
      { name: "Poireaux", quantity: 3, unit: "unité" },
    ],
  },
  {
    name: "Gâteau au yaourt et aux pépites",
    instructions:
      "Utiliser le pot de yaourt comme mesure : 1 yaourt, 2 pots de sucre, 3 de farine, 1/2 d'huile, 3 œufs, un sachet de levure. Ajouter les pépites de chocolat et cuire 30 minutes à 180 °C.",
    ingredients: [
      { name: "Yaourts nature", quantity: 1, unit: "pot" },
      { name: "Farine de blé T65", quantity: 250, unit: "g" },
      { name: "Chocolat noir pâtissier", quantity: 100, unit: "g" },
    ],
  },
  {
    name: "Velouté de courgettes au chèvre",
    instructions:
      "Faire suer l'oignon, ajouter les courgettes en rondelles et couvrir de bouillon. Cuire 20 minutes, mixer finement, incorporer le chèvre frais hors du feu.",
    ingredients: [
      { name: "Courgettes", quantity: 4, unit: "unité" },
      { name: "Chèvre frais", quantity: 1, unit: "bûche" },
    ],
  },
  {
    name: "Boulettes de bœuf à la tomate",
    instructions:
      "Mélanger viande hachée, mie de pain trempée, œuf, persil et ail. Former les boulettes, les colorer, puis les laisser mijoter 25 minutes dans la sauce tomate.",
    ingredients: [
      { name: "Steak haché", quantity: 500, unit: "g" },
      { name: "Tomates pelées", quantity: 1, unit: "boîte" },
      { name: "Pain de campagne au levain", quantity: 1, unit: "tranche" },
    ],
  },
  {
    name: "Tian de légumes d'été",
    instructions:
      "Ranger en rosace les rondelles de courgette, aubergine et tomate sur un lit d'oignons fondus. Huile d'olive, thym, ail, puis 55 minutes à 180 °C.",
    ingredients: [
      { name: "Courgettes", quantity: 2, unit: "unité" },
      { name: "Tomates grappe", quantity: 4, unit: "unité" },
      { name: "Huile d'olive vierge extra", quantity: 4, unit: "cuillère" },
    ],
  },
  {
    name: "Pommes de terre sautées à l'ail",
    instructions:
      "Précuire les pommes de terre 10 minutes à l'eau, les égoutter, puis les faire dorer à la poêle avec beurre et huile. Ajouter ail et persil à la toute fin.",
    ingredients: [
      { name: "Pommes de terre à chair ferme", quantity: 1, unit: "kg" },
      { name: "Ail rose de Lautrec", quantity: 3, unit: "gousse" },
    ],
  },
  {
    name: "Compote pommes-poires sans sucre ajouté",
    instructions:
      "Couper les fruits en morceaux, cuire à couvert avec deux cuillères d'eau et un bâton de cannelle 25 minutes. Écraser à la fourchette pour garder du morceau.",
    ingredients: [
      { name: "Pommes Chantecler", quantity: 4, unit: "unité" },
      { name: "Poires", quantity: 3, unit: "unité" },
    ],
  },
];

export const MESSAGES: readonly string[] = [
  "Je passe au marché en rentrant, il manque quelque chose ?",
  "Pense à sortir la poubelle jaune ce soir, la collecte est demain matin.",
  "Le rendez-vous chez le pédiatre est décalé à 16 h 30, je l'ai mis dans l'agenda.",
  "Il reste du gratin d'hier au frigo, à finir avant demain soir.",
  "J'ai payé la cantine du trimestre, c'est dans le budget.",
  "Le chauffagiste passe jeudi entre 9 h et 11 h, tu peux être là ?",
  "Léa a besoin d'un cahier grands carreaux pour lundi.",
  "On n'a presque plus de lessive, je l'ai ajoutée à la liste.",
  "Le colis est arrivé au relais, il faut le récupérer avant samedi.",
  "Je m'occupe des courses ce week-end si tu prends le match de Nino.",
  "Attention, le lait dans le frigo est périmé depuis hier.",
  "J'ai réservé le gîte en Bretagne pour la deuxième semaine d'août.",
  "Le compteur d'eau est relevé, index noté sur le papier de l'entrée.",
  "On mange quoi ce soir ? Il reste des lentilles et deux saucisses.",
  "Rappel : réunion de copropriété mardi, ordre du jour dans la boîte mail.",
  "J'ai emmené le chat chez le véto, tout va bien, prochain rappel dans un an.",
  "Le lave-vaisselle fait un bruit bizarre depuis ce matin, à surveiller.",
  "Les crampons de Nino sont trop petits, il faut en racheter avant samedi.",
  "J'ai ajouté les recettes de la semaine, la liste de courses est générée.",
  "La facture d'électricité a bien augmenté ce mois-ci, on regarde ensemble ?",
  "Je récupère les enfants à la sortie de l'école aujourd'hui.",
  "Il faut penser à signer l'autorisation pour la sortie au musée.",
  "Le pain est à prendre à la boulangerie avant 19 h, sinon ils ferment.",
  "J'ai décalé le rendez-vous du dentiste à la semaine prochaine.",
  "Stock de café presque vide, j'en rapporte demain.",
  "Le voisin propose son taille-haie ce week-end pour la haie du fond.",
  "Les vacances scolaires commencent le 19, à noter pour la garde.",
  "J'ai payé l'assurance auto, prélèvement passé ce matin.",
  "On a reçu le devis de l'architecte, il faut en discuter calmement.",
  "N'oublie pas ton ordonnance pour la pharmacie.",
  "Le pédiatre a prescrit de la vitamine D, c'est dans la salle de bain.",
  "J'ai rangé les affaires d'hiver au grenier, les cartons sont étiquetés.",
  "Le gâteau d'anniversaire est commandé, à récupérer samedi à 10 h.",
  "Il faut renouveler la licence de judo avec le certificat médical.",
  "J'ai mis les piles neuves dans la télécommande.",
  "Les yaourts sont en promotion cette semaine, j'en ai pris douze.",
  "Le plombier ne peut pas venir avant mardi prochain.",
  "J'ai noté les dépenses du week-end dans le budget, on est dans les clous.",
  "Léa a oublié son doudou chez mamie, on le récupère dimanche.",
  "La médiathèque relance pour les livres, il faut les rendre cette semaine.",
  "J'ai mis de côté les vêtements trop petits pour la brocante.",
  "On refait le plein d'huile d'olive, il n'en reste qu'un fond.",
  "Le contrôle technique est valable jusqu'à la fin du mois, à ne pas oublier.",
  "Je passe à la déchetterie samedi matin, tu as des choses à ajouter ?",
  "Les copains de Léa arrivent à 15 h pour le goûter d'anniversaire.",
  "J'ai réglé la facture internet, tout est à jour côté abonnements.",
  "Le potager a besoin d'eau, il n'a pas plu depuis huit jours.",
  "On garde le poulet pour demain midi, j'ai prévu autre chose ce soir.",
  "J'ai mis à jour la liste de courses avec ce qui manque en stock.",
  "Le spectacle de fin d'année est à 18 h, la salle ouvre 30 minutes avant.",
  "Il reste deux parts de tarte, ne les jetez pas s'il vous plaît.",
  "J'ai déclaré les revenus en ligne, l'accusé de réception est archivé.",
  "Le vélo de Nino a une crevaison, kit de réparation dans le garage.",
  "Les gouttières sont pleines de feuilles, à faire avant les grosses pluies.",
  "Bonne nouvelle : le remboursement de la mutuelle est arrivé.",
];
